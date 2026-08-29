//! A camada P2P: liga peers pela chave pública e sincroniza os logs dos servidores.
//!
//! Não há servidor no meio. Cada instância aceita ligações e liga-se a quem conhece; quando
//! duas se encontram, trocam tudo o que têm dos servidores que partilham. É por isso que o
//! histórico "aparece" quando alguém entra: não foi puxado de lado nenhum, foi trazido por
//! quem já o tinha.
//!
//! A identidade do peer vem do certificado TLS do iroh — já provada pelo transporte, sem
//! handshake nosso por cima. Isso é a diferença face ao caminho do Tor, onde é preciso
//! provar a identidade dentro do protocolo.

use anyhow::{anyhow, bail, Result};
use data_encoding::HEXLOWER;
use iroh::endpoint::{presets, Connection, RecvStream, SendStream};
use iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey};
use serde::{Deserialize, Serialize};
use spike_common::log as blog;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::sync::broadcast;

use crate::estado::App;

pub const ALPN: &[u8] = b"bruma/1";
const MAX_FRAME: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "t")]
pub enum Msg {
    Ola {
        nome: String,
        /// A minha chave de conversa (x25519) e a prova de que é minha.
        ///
        /// Vão como CAMPOS de uma variante que já existia, e não numa variante nova: o
        /// `serde` ignora campos que não conhece, e o `default` cobre o sentido contrário.
        /// Uma variante nova, essa, derrubaria a ligação com qualquer versão anterior.
        ///
        /// São `Option` porque uma versão antiga não os manda — e então simplesmente não há
        /// conversa privada com essa pessoa até ela actualizar.
        #[serde(default)]
        x_pub: Option<String>,
        #[serde(default)]
        prekey_sig: Option<String>,
    },
    /// Tudo o que tenho deste servidor. Quem não o tiver ignora.
    Sync {
        servidor: String,
        entradas: Vec<blog::Entry>,
    },
    Nova {
        servidor: String,
        entrada: blog::Entry,
    },
    /// "Estou (ou deixei de estar) neste canal de voz."
    Presenca {
        servidor: String,
        canal: Option<String>,
    },
    /// Sinalizacao WebRTC. O conteudo e opaco para o Rust: e SDP ou candidatos ICE que
    /// so a webview sabe interpretar. Aqui so se encaminha para o peer certo.
    Sinal {
        servidor: String,
        canal: String,
        dados: String,
    },
    /// Uma mensagem que esta versão não conhece.
    ///
    /// # Porque é que isto tem de existir
    ///
    /// Sem esta variante, o `serde` recusa qualquer `t` que não esteja na lista, o `ler()`
    /// devolve `Err` e o leitor faz `break` — **a sessão inteira cai**. Ou seja: no dia em
    /// que uma versão nova acrescentar uma mensagem, ela deixa de conseguir falar com todas
    /// as versões anteriores. Não é uma funcionalidade que falha; é a ligação que morre.
    ///
    /// Com ela, o que não se conhece é ignorado e a conversa continua. Não salva as versões
    /// já instaladas — salva todas as que vierem a seguir, e é por isso que entra sozinha e
    /// antes de tudo o resto.
    #[serde(other)]
    Desconhecida,
}

/// O que sai daqui para as sessoes abertas.
#[derive(Clone, Debug)]
pub enum Saida {
    Entrada(String, blog::Entry),
    Presenca(String, Option<String>),
    /// Dirigido a UM peer. As outras sessoes ignoram.
    /// «Toma o meu log deste servidor», a UM par.
    ///
    /// Existe porque o sync deixou de ser voluntariado. Quando alguém se dá a conhecer num
    /// servidor que também é meu, respondo-lhe com o que tenho — e é isto que mantém o
    /// convite a funcionar: quem entra fala primeiro, e é a resposta que lhe traz o
    /// histórico. Nunca vai ao fio como variante nova; converte-se num `Msg::Sync` normal.
    SyncPara {
        para: String,
        servidor: String,
    },
    Sinal {
        para: String,
        servidor: String,
        canal: String,
        dados: String,
    },
    /// Um pedaco de ecra, para UM espectador. O `Arc` e de proposito: numa sala com
    /// varios a ver, isto passa por todas as sessoes e so uma o quer -- copiar cem
    /// kilobytes em cada uma delas seria pagar a difusao que estamos a evitar.
    Video {
        /// `"ecra"` ou `"camara"`. Vão pelo mesmo caminho porque são a mesma coisa —
        /// bytes de vídeo com um destino — e separá-los em dois transportes seria manter
        /// duas versões do mesmo código para ganhar nada.
        tipo: &'static str,
        para: String,
        servidor: String,
        canal: String,
        dados: Arc<Vec<u8>>,
    },
}

/// O que passou por uma ligação, para o painel de diagnóstico.
#[derive(Default, Clone, Copy)]
pub struct Contagem {
    pub voz_env: u64,
    pub voz_rec: u64,
    pub ecra_env: u64,
    pub ecra_rec: u64,
}

pub struct Rede {
    pub endpoint: Endpoint,
    /// Entradas criadas localmente, para as sessões abertas difundirem.
    pub tx: broadcast::Sender<Saida>,
    /// Em que sala de voz estamos, se estivermos em alguma.
    ///
    /// A presença era só anunciada quando MUDAVA. Quem se ligasse a seguir nunca ficava a
    /// saber quem já lá estava — e, como só se envia voz a quem se sabe estar na sala, o
    /// som ia num sentido e não no outro. Guardar o estado atual é o que permite contá-lo
    /// a quem chega.
    pub presenca: std::sync::Mutex<Option<(String, Option<String>)>>,
    /// Quantos pedaços de voz saíram e entraram, por peer.
    ///
    /// Existe para o dia em que alguém disser "não se ouve nada". Com isto, a resposta
    /// deixa de ser um palpite: ou não estamos a enviar, ou não estamos a receber, ou o
    /// problema está no som e não na rede — e são três sítios diferentes para procurar.
    pub contagem: std::sync::Mutex<std::collections::HashMap<String, Contagem>>,
    /// As ligações abertas, por peer.
    ///
    /// O resto do módulo trabalha por difusão: escreve-se num canal e cada sessão decide
    /// se lhe diz respeito. Para a voz isso não serve — a voz vai em **datagramas**, que
    /// são enviados na ligação e não num stream, e é preciso ter a ligação à mão.
    /// A ligação viva de cada par, e o número de série da sessão que a pôs lá.
    ///
    /// O número vive DENTRO do mesmo mapa de propósito. Estava à parte, e entre escrever a
    /// ligação nova e escrever o número havia um instante em que a sessão antiga, ao
    /// morrer, se reconhecia como a dona e apagava a ligação NOVA — deixando o par a
    /// parecer desligado com uma sessão viva por baixo. Duas verdades em dois sítios são
    /// duas oportunidades de discordarem.
    pub ligacoes: std::sync::Mutex<std::collections::HashMap<String, (Connection, u64)>>,
    /// Quem já está a ser ligado, para não se ligar duas vezes ao mesmo.
    ///
    /// O guarda de `ligar` lia só o `ligacoes`, e esse só é escrito **depois** da ligação
    /// estar feita. Entre a decisão de ligar e o registo passa quase um segundo, e o
    /// vigia corre de dois em dois: duas chamadas passavam pelo guarda antes de qualquer
    /// uma escrever. O resultado eram DUAS sessões para o mesmo par, cada uma com o seu
    /// escritor — e tudo saía a dobrar, incluindo cada fragmento de ecrã.
    pub a_ligar: std::sync::Mutex<std::collections::HashSet<String>>,
}

impl Rede {
    pub async fn arrancar(app: Arc<App>, janela: AppHandle) -> Result<Arc<Self>> {
        let endpoint = Endpoint::builder(presets::N0)
            .secret_key(SecretKey::from_bytes(&app.semente))
            .alpns(vec![ALPN.to_vec()])
            .bind()
            .await
            .map_err(|e| anyhow!("não consegui abrir a rede: {e}"))?;

        let (tx, _) = broadcast::channel(512);
        let rede = Arc::new(Rede {
            endpoint: endpoint.clone(),
            tx,
            ligacoes: Default::default(),
            a_ligar: Default::default(),
            contagem: Default::default(),
            presenca: Default::default(),
        });

        // Aceitar ligações de quem nos conhece.
        {
            let rede = rede.clone();
            let app = app.clone();
            let janela = janela.clone();
            tokio::spawn(async move {
                while let Some(incoming) = endpoint.accept().await {
                    let (rede, app, janela) = (rede.clone(), app.clone(), janela.clone());
                    tokio::spawn(async move {
                        match incoming.await {
                            Ok(conn) => {
                                if let Err(e) = sessao(conn, false, rede, app, janela).await {
                                    eprintln!("[rede] sessão terminou: {e}");
                                }
                            }
                            Err(e) => eprintln!("[rede] ligação recusada: {e}"),
                        }
                    });
                }
            });
        }

        // Reatar com os peers já conhecidos, sem precisar do convite outra vez — e
        // continuar a tentar enquanto a app viver. Ver `vigiar_ligacoes`.
        {
            let rede = rede.clone();
            let app = app.clone();
            let janela = janela.clone();
            tokio::spawn(async move { vigiar_ligacoes(rede, app, janela).await });
        }

        // O ESTRANHO. Liga-se a alguém com quem não partilha servidor nenhum e conta o que
        // lhe chega — que é a única forma de medir o que sai daqui para quem não devia
        // receber nada. Uma pessoa mal-intencionada só precisa do `EndpointId`, que vai em
        // qualquer convite que eu tenha criado; a bandeira só torna isso reproduzível.
        // Bloquear alguem A MEIO de uma sessao viva -- que e o caso que o bloqueio no
        // arranque nunca poe a prova, porque nessa altura ainda nao ha sessao nenhuma para
        // derrubar. Sem isto, a medicao dizia "esta na lista" e ficava-se por ai.
        #[cfg(debug_assertions)]
        if let Ok(alvo) = std::env::var("BRUMA_BLOQUEIA_TARDE") {
            let app = Arc::clone(&app);
            let rede = Arc::clone(&rede);
            tokio::spawn(async move {
                let espera = std::env::var("BRUMA_BLOQUEIA_TARDE_MS")
                    .ok()
                    .and_then(|x| x.parse().ok())
                    .unwrap_or(25_000);
                tokio::time::sleep(std::time::Duration::from_millis(espera)).await;
                let viva_antes = rede
                    .ligacoes
                    .lock()
                    .map(|l| l.contains_key(alvo.trim()))
                    .unwrap_or(false);
                let r = crate::comandos::aplicar_bloqueio(&app, &rede, alvo.trim(), true);
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                let viva_depois = rede
                    .ligacoes
                    .lock()
                    .map(|l| l.contains_key(alvo.trim()))
                    .unwrap_or(false);
                eprintln!(
                    "[bloqueio-tarde] bloqueou={} sessao-viva-antes={viva_antes} sessao-viva-depois={viva_depois}",
                    r.is_ok()
                );
            });
        }

        #[cfg(debug_assertions)]
        if let Ok(alvo) = std::env::var("BRUMA_ESTRANHO") {
            let rede = rede.clone();
            let app = app.clone();
            let janela = janela.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(4)).await;
                match ligar(&rede, &app, &janela, alvo.trim()).await {
                    Ok(()) => eprintln!("[estranho] liguei-me a {}", &alvo[..8.min(alvo.len())]),
                    Err(e) => eprintln!("[estranho] não consegui ligar: {e}"),
                }
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;

                if let Ok(sala) = std::env::var("BRUMA_ESTRANHO_SALA") {
                    let (srv, canal) = sala.split_once('/').unwrap_or((sala.as_str(), "x"));

                    // O ataque tem de ser em DOIS ACTOS, e foi o que me faltou perceber:
                    // o `aprender_dos_logs` corre uma vez, no arranque da sessao, ANTES do
                    // lixo chegar. Semear e colher na mesma ligacao nunca podia funcionar --
                    // e eu ia concluir que a defesa aguentava.
                    // Acto 1 semeia e sai. Acto 2 volta com a MESMA chave e colhe.
                    let acto: u8 = std::env::var("BRUMA_ESTRANHO_ACTO")
                        .ok()
                        .and_then(|x| x.parse().ok())
                        .unwrap_or(0);

                    // Abrir-lhe uma conversa: era isso que me punha nos peers de um "servidor"
                    // dele e me dava direitos de sala.
                    if acto != 2 {
                        match app.abrir_conversa(alvo.trim()) {
                            Err(e) => eprintln!("[estranho] nao consegui abrir conversa: {e}"),
                            Ok(id) => {
                                // E ESCREVER nela. Abrir do meu lado nao chega -- e a mensagem
                                // que faz a conversa nascer do lado dele, e era isso que me
                                // punha nos peers de um "servidor" dele.
                                let entrada = {
                                    let mut sv = app.servidores.lock().unwrap();
                                    sv.get_mut(&id).and_then(|srv| {
                                        srv.escrever(
                                            &app.ident.signing,
                                            &crate::modelo::Carga::Mensagem {
                                                canal: crate::modelo::CANAL_DA_CONVERSA.into(),
                                                texto: "abre-me a porta".into(),
                                            },
                                        )
                                        .ok()
                                    })
                                };
                                if let Some(e) = entrada {
                                    rede.difundir(&id, e);
                                    eprintln!("[estranho] abri e escrevi numa conversa com ele");
                                }
                            }
                        }
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

                        // O LIXO ASSINADO. Escrevo no meu proprio servidor -- a entrada fica
                        // assinada por mim e cifrada com a MINHA chave, que ele nao tem. Depois
                        // mando-lha com o id da sala DELE a frente. O `merge` so verifica a
                        // assinatura, portanto ela entra no ficheiro do log dele com o meu
                        // `author` em claro. Na ligacao seguinte, o `aprender_dos_logs` lia esse
                        // `author` e adoptava-me.
                        let lixo = {
                            let mut sv = app.servidores.lock().unwrap();
                            sv.values_mut().find(|x| x.com.is_none()).and_then(|srv| {
                                srv.escrever(
                                    &app.ident.signing,
                                    &crate::modelo::Carga::Mensagem {
                                        canal: "x".into(),
                                        texto: "lixo".into(),
                                    },
                                )
                                .ok()
                            })
                        };
                        if let Some(e) = lixo {
                            let _ = rede.tx.send(Saida::Entrada(srv.to_string(), e));
                            eprintln!("[estranho] semeei lixo assinado por mim na sala dele");
                            tokio::time::sleep(std::time::Duration::from_secs(4)).await;
                        }
                    }

                    if acto != 1 {
                        // Primeiro dizer o nome da sala dele, que era o que bastava para eu ficar
                        // inscrito nos peers — e a partir daí passar por membro em tudo o resto.
                        let _ = rede.tx.send(Saida::SyncPara {
                            para: alvo.trim().to_string(),
                            servidor: srv.to_string(),
                        });
                        eprintln!("[estranho] disse o nome da sala {srv} sem ter a chave dela");
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

                        // E agora forjar presença. Se a inscrição pegou, isto passa o porteiro.
                        let _ = rede
                            .tx
                            .send(Saida::Presenca(srv.to_string(), Some(canal.to_string())));
                        eprintln!("[estranho] forjei presença em {srv}/{canal}");
                    }
                }

                // E injectar som nas colunas dele.
                for _ in 0..40 {
                    rede.enviar_voz(&[alvo.trim().to_string()], b"som que ninguem pediu");
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                eprintln!("[estranho] mandei 40 pedaços de voz");
            });
        }

        Ok(rede)
    }

    pub fn id(&self) -> EndpointId {
        self.endpoint.id()
    }

    /// Difunde uma entrada nova para todas as sessões abertas.
    pub fn difundir(&self, servidor: &str, entrada: blog::Entry) {
        // Falha em silêncio se ninguém estiver ligado, e está certo: fica no log local
        // e vai no próximo sync de quem aparecer.
        let _ = self.tx.send(Saida::Entrada(servidor.to_string(), entrada));
    }

    pub fn anunciar_presenca(&self, servidor: &str, canal: Option<String>) {
        if let Ok(mut p) = self.presenca.lock() {
            *p = Some((servidor.to_string(), canal.clone()));
        }
        let _ = self.tx.send(Saida::Presenca(servidor.to_string(), canal));
    }

    /// Um pedaco de ecra para cada espectador. Quem nao carregou em "Assistir" nao
    /// aparece nesta lista e nao recebe nada -- e a diferenca entre gastar uma copia de
    /// upload e gastar seis.
    /// Manda um pedaço de voz a cada pessoa da sala, em datagramas.
    ///
    /// **Datagramas e não streams, e a diferença é toda.** Um stream QUIC é fiável e
    /// ordenado: se um pacote se perde, tudo o que vem atrás fica à espera dele. Para um
    /// ficheiro é o que se quer; para voz ao vivo é o pior possível — o atraso acumula e
    /// nunca mais encolhe. Um pacote de voz perdido não vale a pena reenviar, porque
    /// quando chegasse já tinha passado a vez dele.
    ///
    /// Falhas aqui são normais e não se registam: um datagrama grande de mais ou um buffer
    /// cheio significa que aquele bocado de som se perdeu, e é assim que deve ser.
    pub fn enviar_voz(&self, para: &[String], dados: &[u8]) {
        let Ok(ligacoes) = self.ligacoes.lock() else {
            return;
        };
        let bytes = bytes::Bytes::copy_from_slice(dados);
        for p in para {
            if let Some((c, _)) = ligacoes.get(p) {
                if c.send_datagram(bytes.clone()).is_ok() {
                    if let Ok(mut n) = self.contagem.lock() {
                        n.entry(p.clone()).or_default().voz_env += 1;
                    }
                }
            }
        }
    }

    pub fn enviar_video(&self, para: &[String], servidor: &str, canal: &str, dados: Arc<Vec<u8>>) {
        for p in para {
            let _ = self.tx.send(Saida::Video {
                tipo: "ecra",
                para: p.clone(),
                servidor: servidor.to_string(),
                canal: canal.to_string(),
                dados: dados.clone(),
            });
        }
    }

    /// A câmara, pelo mesmo caminho do ecrã.
    ///
    /// Podia ir em datagramas, como a voz, mas não vai — e a razão é o tamanho. Um pedaço
    /// de voz tem umas dezenas de bytes e cabe num datagrama; um frame-chave de câmara tem
    /// dezenas de KILObytes e teria de ser partido em vinte, com reconstrução do outro
    /// lado e um frame inteiro perdido por cada pedaço que faltasse. Num stream fiável isso
    /// não existe. O preço é o bloqueio de cabeça de linha, que a este débito não se nota.
    pub fn enviar_camara(&self, para: &[String], servidor: &str, canal: &str, dados: Arc<Vec<u8>>) {
        for p in para {
            let _ = self.tx.send(Saida::Video {
                tipo: "camara",
                para: p.clone(),
                servidor: servidor.to_string(),
                canal: canal.to_string(),
                dados: dados.clone(),
            });
        }
    }

    pub fn enviar_sinal(&self, para: &str, servidor: &str, canal: &str, dados: String) {
        let _ = self.tx.send(Saida::Sinal {
            para: para.to_string(),
            servidor: servidor.to_string(),
            canal: canal.to_string(),
            dados,
        });
    }
}

/// Mantém as ligações de pé, para sempre.
///
/// # A avaria que isto corrige
///
/// Antes, ligava-se aos peers conhecidos **uma vez, no arranque** — e o erro era deitado
/// fora sem sequer um registo. Quando a ligação caía, abortavam-se as tarefas, removia-se
/// o peer, emitia-se `peer-desligado`, e acabava. **Nunca mais se tentava.**
///
/// As mensagens recuperavam-se sozinhas (o log converge no próximo sync), mas a voz e o
/// ecrã morriam de vez até alguém reiniciar a app. Numa ligação entre o Brasil e os EUA,
/// dois segundos de Wi-Fi a falhar não são hipótese — são terça-feira.
///
/// # Porquê um vigia só, e não uma tentativa por queda
///
/// Um vigia único é simples de raciocinar e cura sozinho casos que uma tentativa por queda
/// não cobre: a app arrancou sem rede nenhuma, o peer só apareceu mais tarde, ou a queda
/// aconteceu antes de haver sessão. Se um dia falhar, falha de uma maneira só.
///
/// O recuo é exponencial por peer — de 2 até 60 segundos — e volta ao princípio assim que
/// a ligação pega. Sem recuo, um peer desligado durante a noite dava milhares de tentativas
/// e enchia o registo de ruído.
async fn vigiar_ligacoes(rede: Arc<Rede>, app: Arc<App>, janela: AppHandle) {
    use std::collections::HashMap;
    let mut espera: HashMap<String, u64> = HashMap::new();
    let mut proxima: HashMap<String, std::time::Instant> = HashMap::new();
    loop {
        let conhecidos: Vec<String> = {
            let Ok(s) = app.servidores.lock() else {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                continue;
            };
            let mut v: Vec<String> = s.values().flat_map(|x| x.peers.clone()).collect();
            // E quem me convidou. Ele ainda não provou nada — e por isso não passa o
            // porteiro — mas é o único fio que tenho para o servidor onde acabei de entrar.
            v.extend(s.values().filter_map(|x| x.convidou.clone()));
            // E os amigos, que podem não partilhar sala nenhuma comigo. Sem isto, ter alguém
            // na lista não servia de nada: nunca nos ligaríamos, e a conversa privada com
            // quem não está num servidor meu nunca chegaria a acontecer.
            if let Ok(a) = app.amigos.lock() {
                v.extend(a.iter().map(|x| x.chave.clone()));
            }
            v.sort();
            v.dedup();
            v
        };
        // TIRAR OS BLOQUEADOS DA LISTA DE QUEM DISCAR.
        //
        // Bloquear escreve na lista e fecha a ligação viva — e mais nada. A pessoa continua
        // em `srv.peers` de todas as salas, e o vigia continuava a ligar-se-lhe de dois em
        // dois segundos. A sessão morria no primeiro `if e_bloqueado`, o vigia voltava a
        // tentar, e ficava um ciclo a encher o registo com tentativas a alguém que eu
        // recusei — e a dizer-lhe, com o próprio tráfego, que eu estou online.
        let conhecidos: Vec<String> = conhecidos
            .into_iter()
            .filter(|p| !app.e_bloqueado(p))
            .collect();
        let agora = std::time::Instant::now();
        for peer in conhecidos {
            let ja_ligado = rede
                .ligacoes
                .lock()
                .map(|l| l.contains_key(&peer))
                .unwrap_or(false);
            if ja_ligado {
                // Pegou: o próximo corte volta a tentar depressa, e não daqui a um minuto.
                espera.remove(&peer);
                proxima.remove(&peer);
                continue;
            }
            if proxima.get(&peer).is_some_and(|q| agora < *q) {
                continue;
            }
            let s = espera.entry(peer.clone()).or_insert(2);
            match ligar(&rede, &app, &janela, &peer).await {
                Ok(()) => {
                    eprintln!("[rede] religado a {}", &peer[..8.min(peer.len())]);
                    espera.remove(&peer);
                    proxima.remove(&peer);
                }
                Err(e) => {
                    eprintln!(
                        "[rede] {} não atendeu ({e}); nova tentativa daqui a {s}s",
                        &peer[..8.min(peer.len())]
                    );
                    proxima.insert(peer.clone(), agora + std::time::Duration::from_secs(*s));
                    *s = (*s * 2).min(60);
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

/// Reserva o par para uma tentativa de ligação. `false` quer dizer «já há, ou já vai a
/// caminho» — e nesse caso não se liga outra vez.
fn reservar(
    ligacoes: &std::sync::Mutex<std::collections::HashMap<String, (Connection, u64)>>,
    a_ligar: &std::sync::Mutex<std::collections::HashSet<String>>,
    peer: &str,
) -> bool {
    let ja = ligacoes
        .lock()
        .map(|l| l.contains_key(peer))
        .unwrap_or(false);
    let Ok(mut fila) = a_ligar.lock() else {
        return false;
    };
    !ja && fila.insert(peer.to_string())
}

/// Qual das duas ligações sobrevive quando os dois lados se ligam ao mesmo tempo.
///
/// Tem de dar o **mesmo resultado nas duas máquinas**. Se cada uma ficasse com a sua, cada
/// uma escrevia numa ligação que a outra não lia — dois pares ligados e mudos. Por isso a
/// regra é sobre uma coisa que ambas veem igual: sobrevive a ligação que o id MENOR
/// iniciou.
fn fica_esta(eu: &str, peer: &str, iniciei: bool) -> bool {
    iniciei == (eu < peer)
}

pub async fn ligar(rede: &Arc<Rede>, app: &Arc<App>, janela: &AppHandle, peer: &str) -> Result<()> {
    let id: EndpointId = peer
        .trim()
        .parse()
        .map_err(|_| anyhow!("identificador de peer inválido"))?;
    if id == rede.endpoint.id() {
        bail!("esse é o teu próprio identificador");
    }
    // Daqui para baixo usa-se a forma canónica, e não o que nos deram. A reserva é posta
    // aqui e levantada pela sessão, que só conhece o par por `remote_id()`: se as duas
    // formas diferirem numa maiúscula ou num espaço, a reserva fica presa para sempre e o
    // par nunca mais é tentado -- uma avaria permanente nascida de uma diferença de texto.
    let peer = &id.to_string();
    // Já ligado, ou a ligar: não se abre uma segunda. A reserva tem de acontecer AGORA e
    // não quando a ligação ficar pronta — ver o comentário de `a_ligar`.
    // Não se disca para quem se bloqueou: o vigia lê a lista dos servidores, e um bloqueado
    // pode continuar lá dentro.
    if app.e_bloqueado(peer) {
        return Ok(());
    }
    if !reservar(&rede.ligacoes, &rede.a_ligar, peer) {
        return Ok(());
    }
    let conn = match rede.endpoint.connect(EndpointAddr::from(id), ALPN).await {
        Ok(c) => c,
        Err(e) => {
            // A reserva morre com a tentativa, senão o par ficava marcado para sempre e
            // nunca mais se tentava — uma falha de rede passaria a ser permanente.
            if let Ok(mut fila) = rede.a_ligar.lock() {
                fila.remove(peer);
            }
            return Err(anyhow!("não consegui ligar: {e}"));
        }
    };
    let (rede, app, janela) = (rede.clone(), app.clone(), janela.clone());
    tokio::spawn(async move {
        if let Err(e) = sessao(conn, true, rede, app, janela).await {
            eprintln!("[rede] sessão terminou: {e}");
        }
    });
    Ok(())
}

/// O que tem de acontecer quando uma sessão acaba — aconteça ela acabar como for.
///
/// # A avaria que isto corrige
///
/// A limpeza estava escrita no FIM da `sessao`, e entre o registo no mapa `ligacoes` e esse
/// fim há cinco `?`: abrir o stream, o `Ola`, cada `Sync` e a `Presenca`. O `Sync` manda o
/// histórico inteiro de cada servidor — com uns milhares de mensagens demora, e é
/// exactamente a janela em que um soluço de Wi-Fi é provável.
///
/// Se algum desses `?` saísse, a entrada ficava no mapa a apontar para uma ligação MORTA. E
/// aí o par ficava inalcançável **para sempre**: o vigia da religação vê `contains_key` e
/// conclui «já está ligado», o `reservar` recusa, e até colar o convite outra vez deixa de
/// fazer nada. As mensagens não chegam, a voz sai para uma ligação fechada e o ecrã nunca
/// vai — até alguém reiniciar a app. É o sintoma que a religação automática existe para
/// curar, ressuscitado por outra porta.
///
/// Um `Drop` corre em todas as saídas. Uma limpeza escrita à mão no fim só corre numa.
struct SessaoViva {
    rede: Arc<Rede>,
    janela: AppHandle,
    peer: String,
    serie: u64,
    tarefas: Vec<tokio::task::JoinHandle<()>>,
    /// Se já dissemos à interface que este par ligou.
    ///
    /// O `peer-ligado` sai depois de haver stream; se a sessão morrer antes disso, ninguém
    /// contou este par e um `peer-desligado` faria o contador da barra descer a menos do
    /// que deve — a interface soma e subtrai eventos, não olha para o mapa.
    anunciado: bool,
}

impl Drop for SessaoViva {
    fn drop(&mut self) {
        for t in &self.tarefas {
            t.abort();
        }
        // Só se apaga do mapa se a ligação que lá está for ESTA. Ver e apagar têm de ser o
        // mesmo gesto: se entretanto entrou outra sessão, apagá-la deixava o par a parecer
        // desligado com uma ligação viva por baixo.
        let era_a_nossa = self
            .rede
            .ligacoes
            .lock()
            .map(|mut l| {
                if l.get(&self.peer).map(|(_, s)| *s) == Some(self.serie) {
                    l.remove(&self.peer);
                    true
                } else {
                    false
                }
            })
            .unwrap_or(false);
        if era_a_nossa && self.anunciado {
            let _ = self.janela.emit("peer-desligado", &self.peer);
        }
    }
}

/// `iniciei` diz se fomos nós a ligar-nos ou se foi o outro lado.
///
/// Não é um detalhe: abrir um stream QUIC é uma ação **local**, que quase sempre funciona.
/// A versão anterior tentava `open_bi()` e só caía no `accept_bi()` se falhasse — e como
/// nunca falha, os dois lados abriam o seu próprio stream, escreviam nele, e ficavam à
/// espera no seu, que ninguém do outro lado alimentava. Dois pares ligavam-se e não
/// trocavam uma única palavra, sem erro nenhum.
///
/// Não apareceu antes porque nunca tinham estado duas instâncias ligadas de verdade.
async fn sessao(
    conn: Connection,
    iniciei: bool,
    rede: Arc<Rede>,
    app: Arc<App>,
    janela: AppHandle,
) -> Result<()> {
    // O par sabe-se assim que a ligação existe, antes de haver stream nenhum. Registar
    // aqui — e não lá em baixo — é o que fecha a janela em que duas sessões coexistem.
    let peer = conn.remote_id().to_string();

    // O BLOQUEIO É A PRIMEIRA COISA, antes de tudo o resto.
    //
    // Fecha-se a ligação sem uma palavra: sem `Ola`, sem nome, sem sequer o `peer-ligado`.
    // Assim ele não consegue distinguir estar bloqueado de eu estar desligado — o que é mais
    // do que o Discord dá, e é a única vantagem de não haver servidor a informá-lo.
    //
    // E fica aqui em cima e não mais abaixo porque um guarda que corre depois de já se ter
    // dito «olá» já disse «olá».
    if app.e_bloqueado(&peer) {
        // SEM RAZAO NENHUMA, e isso e a funcionalidade.
        //
        // O painel promete que ele nao distingue estar bloqueado de eu estar
        // desligado. Mas o QUIC leva a razao do `close` ate ao outro lado, e o
        // Bruma escreve o que lhe chega no registo: a palavra "bloqueado"
        // aterrava no bruma.log dele. A promessa era falsa e a app e que a
        // desmentia.
        //
        // Uma ligacao que fecha sem razao e indistinguivel de uma que caiu.
        conn.close(0u32.into(), b"");
        if let Ok(mut fila) = rede.a_ligar.lock() {
            fila.remove(&peer);
        }
        return Ok(());
    }

    if let Ok(mut fila) = rede.a_ligar.lock() {
        fila.remove(&peer);
    }
    let serie = {
        static PROXIMA: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        PROXIMA.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    };
    // Se os dois lados se ligarem ao mesmo tempo, ficam com uma ligação cada um e nenhuma
    // das duas serve: cada um escreve na sua e lê na sua, e o outro não está lá. O
    // desempate tem de dar o MESMO resultado nas duas máquinas, por isso é pelo
    // identificador: sobrevive sempre a ligação que o id menor iniciou.
    let canonica = fica_esta(&rede.endpoint.id().to_string(), &peer, iniciei);
    enum Destino {
        Fica,
        Substitui(Connection),
        Sobra,
    }
    let destino = {
        let Ok(mut l) = rede.ligacoes.lock() else {
            bail!("mapa de ligações partido");
        };
        match l.get(&peer) {
            None => {
                l.insert(peer.clone(), (conn.clone(), serie));
                Destino::Fica
            }
            Some(_) if !canonica => Destino::Sobra,
            Some((v, _)) => {
                let v = v.clone();
                l.insert(peer.clone(), (conn.clone(), serie));
                Destino::Substitui(v)
            }
        }
    };
    match destino {
        // Já há uma sessão melhor com este par. Esta fecha-se caladamente.
        Destino::Sobra => {
            conn.close(0u32.into(), b"duplicada");
            return Ok(());
        }
        Destino::Substitui(v) => v.close(0u32.into(), b"substituida"),
        Destino::Fica => {}
    }

    // O guarda nasce aqui, colado ao registo, e não mais abaixo: abrir o stream já é uma
    // das saídas que deixava a ligação morta no mapa para sempre.
    let mut guarda = SessaoViva {
        rede: rede.clone(),
        janela: janela.clone(),
        peer: peer.clone(),
        serie,
        tarefas: Vec::new(),
        anunciado: false,
    };

    // Uma sessão que morre ENTRE o registo e o stream é o caminho que deixava um par
    // inalcançável para sempre. Nesta máquina não acontece — a rede local não soluça —
    // por isso força-se, que é a única forma de o ramo deixar de estar por verificar.
    if crate::bandeiras::sessao_morre() {
        use std::sync::atomic::{AtomicBool, Ordering};
        static JA: AtomicBool = AtomicBool::new(false);
        if !JA.swap(true, Ordering::Relaxed) {
            bail!("sessão morta de propósito (BRUMA_SESSAO_MORRE)");
        }
    }

    // Quem liga abre o stream; quem aceita espera por ele. Um de cada lado, e só um.
    let (mut envia, mut recebe) = if iniciei {
        conn.open_bi()
            .await
            .map_err(|e| anyhow!("não consegui abrir o stream: {e}"))?
    } else {
        conn.accept_bi()
            .await
            .map_err(|e| anyhow!("sem stream: {e}"))?
    };

    // Antes de decidir o que sai e o que entra: ver se este par já escreveu numa sala
    // minha. Se escreveu, é de casa, mesmo que nunca tenhamos sincronizado um com o outro.
    aprender_dos_logs(&app, &peer);

    // OUTRA VEZ, DEPOIS DE INSCRITO.
    //
    // A verificação lá em cima acontece antes de esta sessão existir no mapa `ligacoes`. Um
    // bloqueio que caísse nessa janela não encontrava nada para fechar — o `aplicar_bloqueio`
    // procura no mapa — e a sessão seguia viva apesar de a pessoa estar na lista. A janela é
    // pequena e uma ligação dura horas: um bloqueio que não pega é um bloqueio que não
    // existe.
    if app.e_bloqueado(&peer) {
        conn.close(0u32.into(), b"");
        return Ok(());
    }

    let _ = janela.emit("peer-ligado", &peer);
    guarda.anunciado = true;

    // A voz chega por aqui, fora do stream de controlo: um datagrama por pedaço de som.
    let voz_conn = conn.clone();
    let voz_peer = peer.clone();
    let contagem = rede.clone();
    let voz_app = app.clone();
    let ouvinte = tokio::spawn(async move {
        // Uma linha por sessão, e não uma por datagrama: a voz vem cinquenta vezes por
        // segundo, e um registo que se enche a si próprio deixa de ser lido.
        let mut avisei_voz = false;
        loop {
            let d = match voz_conn.read_datagram().await {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("[rede] fim dos datagramas de {voz_peer}: {e}");
                    break;
                }
            };
            // Som de quem não partilha sala nenhuma comigo não toca nas minhas colunas.
            // A interface só verificava se EU estava numa chamada; bastava isso para um
            // estranho ligado me tocar o que quisesse enquanto eu falava com outra pessoa.
            if !conhecido(&voz_app, &voz_peer) {
                if !avisei_voz {
                    avisei_voz = true;
                    eprintln!(
                        "[porteiro] recusei som de {}: não partilha sala nenhuma comigo",
                        &voz_peer[..8.min(voz_peer.len())]
                    );
                }
                continue;
            }
            if let Ok(mut n) = contagem.contagem.lock() {
                n.entry(voz_peer.clone()).or_default().voz_rec += 1;
            }
            crate::comandos::voz_recebida(&voz_peer, &d);
        }
    });

    guarda.tarefas.push(ouvinte);

    let meu_nome = app.nome.lock().unwrap().clone();
    escrever(
        &mut envia,
        &Msg::Ola {
            nome: meu_nome,
            x_pub: Some(HEXLOWER.encode(app.ident.x_public().as_bytes())),
            prekey_sig: Some(HEXLOWER.encode(&app.ident.prekey_signature())),
        },
    )
    .await?;

    // A subscrição vem ANTES da fotografia do log, e não depois de a mandar.
    //
    // Estava lá em baixo, à porta do ciclo. Entre tirar a fotografia e chegar lá passa o
    // tempo de escrever o histórico inteiro de cada servidor — e tudo o que fosse escrito
    // nesse intervalo caía no vazio: não ia na fotografia, porque foi escrito depois dela,
    // e não ia pelo canal, porque ainda não havia quem o ouvisse. A pessoa escrevia uma
    // mensagem e o outro lado só a via na próxima vez que se ligassem.
    //
    // Subscrever primeiro faz o contrário: o que aparecer durante o sync chega DUAS vezes,
    // e isso não custa nada — o log é endereçado pelo conteúdo e o `merge` ignora o que já
    // lá está. Entre perder e repetir, repete-se.
    let mut sub = rede.tx.subscribe();

    // Manda o que é DELE, e não tudo o que tenho.
    //
    // Isto era «manda tudo o que temos; quem não tiver o servidor ignora», com o argumento
    // de ser mais simples do que negociar. Era mais simples e entregava a qualquer par
    // ligado o histórico cifrado de todos os meus servidores. O conteúdo ia protegido, mas
    // o id do servidor, as chaves de quem escreveu, as horas e o volume iam em claro — ou
    // seja, qualquer conhecido ficava a saber em que salas eu estou, com quem, e quando.
    //
    // A regra passa a ser: **não voluntariar, retribuir.** Só se manda o log dos servidores
    // onde este par já é conhecido; quem chega de novo dá-se a conhecer primeiro e recebe a
    // resposta em `aplicar` (ver `Saida::SyncPara`). O convite continua a funcionar porque
    // quem entra tem o anfitrião na lista e fala primeiro.
    let pacotes: Vec<(String, Vec<blog::Entry>)> = {
        let s = app.servidores.lock().unwrap();
        s.values()
            .filter(|srv| {
                srv.peers.iter().any(|p| p == &peer) || srv.convidou.as_deref() == Some(&peer)
            })
            .map(|srv| (srv.id.clone(), srv.log.ordered()))
            .collect()
    };
    for (servidor, entradas) in pacotes {
        // Um sync real de milhares de mensagens demora; nesta maquina e instantaneo, e a
        // janela em que uma mensagem nova se podia perder fecharia sozinha sem nada provar.
        // Alarga-se de proposito para o teste de par a poder medir.
        if let Some(ms) = crate::bandeiras::sync_lento_ms() {
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
        }
        escrever(&mut envia, &Msg::Sync { servidor, entradas }).await?;
    }

    // E dizer onde estamos agora. Sem isto, quem chega depois de nós entrarmos numa sala
    // não sabe que lá estamos, e não nos manda voz nenhuma.
    // ...mas só a quem é dessa sala. Isto ia para qualquer par que se ligasse, e levava o id
    // do servidor E o da sala de voz onde estou neste momento — a um estranho, no primeiro
    // segundo da ligação. Com esses dois valores ele devolvia-me a mesma presença e passava a
    // constar da minha lista de presentes.
    let onde = rede.presenca.lock().ok().and_then(|p| p.clone());
    if let Some((servidor, canal)) = onde {
        if canal.is_some() && participa(&app, &servidor, &peer) {
            escrever(&mut envia, &Msg::Presenca { servidor, canal }).await?;
        }
    }

    let rede_leitura = rede.clone();
    let leitura_app = app.clone();
    let leitura_janela = janela.clone();
    let peer_leitura = peer.clone();
    let mut leitor = tokio::spawn(async move {
        // O `Ola` aceita-se UMA vez por sessão (#141). Cada um que chega chama `guardar_prekey`,
        // e uma prekey diferente força um `gravar_indice` — o ciclo completo de cifrar e
        // escrever no disco. Nada obriga o outro lado a mandar um só: mil `Ola`, cada um com
        // uma chave x25519 que ele assina, eram mil reescritas do índice à cadência dele.
        let mut ola_visto = false;
        loop {
            match ler(&mut recebe).await {
                // O video nao passa pelo emit normal do Tauri: um Vec<u8> vira um array
                // JSON de numeros, que para cem kilobytes de ecra e absurdo. Vai por um
                // canal proprio, em bruto.
                Ok(Quadro::Video {
                    tipo,
                    servidor,
                    canal,
                    dados,
                }) => {
                    // Imagem de um estranho não abre descodificadores meus. Cada fluxo novo
                    // custa um MediaSource, um <video> e um descodificador de hardware; a
                    // interface criava-os para qualquer chave que mandasse bytes.
                    if !conhecido(&leitura_app, &peer_leitura) {
                        continue;
                    }
                    if let Ok(mut n) = rede_leitura.contagem.lock() {
                        n.entry(peer_leitura.clone()).or_default().ecra_rec += 1;
                    }
                    if tipo == "camara" {
                        crate::comandos::camara_recebida(&peer_leitura, dados);
                    } else {
                        crate::comandos::ecra_recebido(&peer_leitura, &servidor, &canal, dados);
                    }
                }
                Ok(Quadro::Controlo(Msg::Ola {
                    nome,
                    x_pub,
                    prekey_sig,
                })) => {
                    let _ = leitura_janela.emit("peer-nome", (&peer_leitura, &nome));
                    if ola_visto {
                        // Um par que manda dois `Ola` na mesma sessão está avariado ou a
                        // experimentar — vale a pena sabê-lo, e não vale gravar o disco por ele.
                        eprintln!(
                            "[rede] {} mandou outro Olá na mesma sessão; ignorado",
                            &peer_leitura[..8.min(peer_leitura.len())]
                        );
                    } else {
                        ola_visto = true;
                        // A chave de conversa dele, guardada para se poder abrir a conversa
                        // mais tarde sem ele estar online. Verificada antes de guardada — sem
                        // isso, qualquer um anunciava a prekey de outro e lia-lhe as conversas.
                        if let (Some(x), Some(sig)) = (x_pub, prekey_sig) {
                            if let Err(e) = leitura_app.guardar_prekey(&peer_leitura, &x, &sig) {
                                eprintln!(
                                    "[rede] a chave de conversa de {} não confere: {e}",
                                    &peer_leitura[..8.min(peer_leitura.len())]
                                );
                            }
                        }
                    }
                }
                // Uma mensagem de uma versão mais nova do que esta. Ignora-se e segue-se:
                // o que não se conhece não pode ser tratado, mas também não é razão para
                // derrubar a ligação. Fica no registo porque, se um dia alguém disser «ele
                // está online mas não recebo nada», é aqui que a resposta aparece.
                Ok(Quadro::Controlo(Msg::Desconhecida)) => {
                    eprintln!(
                        "[rede] {} falou uma coisa que esta versão não conhece; ignorada",
                        &peer_leitura[..8.min(peer_leitura.len())]
                    );
                }
                Ok(Quadro::Controlo(Msg::Sync { servidor, entradas })) => {
                    if ataque_permitido() {
                        eprintln!(
                            "[estranho] recebi um SYNC do servidor {} com {} entradas",
                            &servidor[..8.min(servidor.len())],
                            entradas.len()
                        );
                    }
                    aplicar(
                        &leitura_app,
                        &leitura_janela,
                        &servidor,
                        entradas,
                        &peer_leitura,
                        &rede_leitura,
                    );
                }
                Ok(Quadro::Controlo(Msg::Nova { servidor, entrada })) => {
                    aplicar(
                        &leitura_app,
                        &leitura_janela,
                        &servidor,
                        vec![entrada],
                        &peer_leitura,
                        &rede_leitura,
                    );
                }
                Ok(Quadro::Controlo(Msg::Presenca { servidor, canal })) => {
                    // A presença é o que faz o meu microfone e a minha câmara passarem a
                    // sair para alguém: a interface põe quem se anuncia na lista de
                    // presentes, e é essa lista que decide para quem se envia. Sem porteiro,
                    // bastava a um estranho devolver-me o id do meu próprio canal — que eu
                    // lhe tinha anunciado — para passar a receber-me.
                    if !participa(&leitura_app, &servidor, &peer_leitura) {
                        eprintln!(
                            "[porteiro] recusei presença de {} em {}: não é dessa sala",
                            &peer_leitura[..8.min(peer_leitura.len())],
                            &servidor[..8.min(servidor.len())]
                        );
                        continue;
                    }
                    let _ = leitura_janela.emit(
                        "presenca",
                        serde_json::json!({ "peer": &peer_leitura, "servidor": servidor, "canal": canal }),
                    );
                }
                Ok(Quadro::Controlo(Msg::Sinal {
                    servidor,
                    canal,
                    dados,
                })) => {
                    // O `Sinal` não tinha porteiro nenhum, e é ele que a interface usa para
                    // saber quem entende câmara e quem pediu para assistir — ou seja, é por
                    // aqui que se decide para quem sai a minha câmara e o meu ecrã. Fechei a
                    // voz, o vídeo e a presença e deixei esta porta aberta ao lado.
                    if !participa(&leitura_app, &servidor, &peer_leitura) {
                        eprintln!(
                            "[porteiro] recusei um sinal de {}: não é dessa sala",
                            &peer_leitura[..8.min(peer_leitura.len())]
                        );
                        continue;
                    }
                    let _ = leitura_janela.emit(
                        "sinal",
                        serde_json::json!({ "de": &peer_leitura, "servidor": servidor, "canal": canal, "dados": dados }),
                    );
                }
                Err(_) => break,
            }
        }
    });

    loop {
        tokio::select! {
            // Só o leitor de controlo decide o fim. O de datagramas acabar não é motivo
            // para fechar nada: se ele parar, perde-se a voz — não o servidor inteiro. Ter
            // os dois aqui fazia com que qualquer soluço na voz levasse o chat com ele.
            _ = &mut leitor => break,
            got = sub.recv() => match got {
                Ok(saida) => {
                    let quadro = match saida {
                        // Uma entrada nova só vai a quem participa NESTE servidor. Antes
                        // ia a toda a gente ligada, com o id do servidor à frente: quem
                        // não tinha a chave não lia o conteúdo, mas ficava a saber que eu
                        // acabara de escrever, onde, e a que horas.
                        Saida::Entrada(servidor, entrada) => {
                            // A mesma excepção do simulador de ataque que já existe para a
                            // presença: sem ela o atacante é travado à SAÍDA, o alvo nunca
                            // chega a ser posto à prova, e conclui-se que a defesa aguenta
                            // quando o ataque nem saiu de casa. Foi o que me aconteceu três
                            // vezes hoje.
                            let ataque = ataque_permitido();
                            if ataque || pode_sincronizar(&app, &servidor, &peer) {
                                Some(Quadro::Controlo(Msg::Nova { servidor, entrada }))
                            } else {
                                None
                            }
                        }
                        // A presença diz em que sala de voz estou. Mesma regra: só a quem
                        // é dessa casa.
                        Saida::Presenca(servidor, canal) => {
                            // O `||` existe para o simulador de ataque poder anunciar uma
                            // presença que não lhe pertence. Sem ele o atacante era travado
                            // à saída e o porteiro do outro lado nunca chegava a ser posto
                            // à prova — que é o erro de medir a defesa com um ataque que
                            // nunca sai de casa.
                            let ataque = ataque_permitido();
                            if ataque || participa(&app, &servidor, &peer) {
                                Some(Quadro::Controlo(Msg::Presenca { servidor, canal }))
                            } else {
                                None
                            }
                        }
                        // A resposta a quem se deu a conhecer: o log deste servidor, e só
                        // para ele.
                        Saida::SyncPara { para, servidor } if para == peer => {
                            let entradas = app
                                .servidores
                                .lock()
                                .ok()
                                .and_then(|s| s.get(&servidor).map(|srv| srv.log.ordered()));
                            match entradas {
                                Some(entradas) => {
                                    Some(Quadro::Controlo(Msg::Sync { servidor, entradas }))
                                }
                                // Simulador de ataque: mandar um `Sync` de um servidor que
                                // NÃO tenho, só para ver se o outro lado me inscreve por eu
                                // dizer o nome. Era assim que o porteiro se contornava.
                                None if ataque_permitido() => Some(
                                    Quadro::Controlo(Msg::Sync {
                                        servidor,
                                        entradas: Vec::new(),
                                    }),
                                ),
                                None => None,
                            }
                        }
                        Saida::SyncPara { .. } => None,
                        // Sinalizacao e video sao dirigidos: as outras sessoes deixam passar.
                        Saida::Sinal { para, servidor, canal, dados } if para == peer => {
                            Some(Quadro::Controlo(Msg::Sinal { servidor, canal, dados }))
                        }
                        Saida::Sinal { .. } => None,
                        Saida::Video { tipo, para, servidor, canal, dados } if para == peer => {
                            if let Ok(mut n) = rede.contagem.lock() {
                                n.entry(peer.clone()).or_default().ecra_env += 1;
                            }
                            Some(Quadro::Video {
                                tipo: tipo.to_string(),
                                servidor,
                                canal,
                                dados: dados.as_ref().clone(),
                            })
                        }
                        Saida::Video { .. } => None,
                    };
                    if let Some(q) = quadro {
                        if escrever_quadro(&mut envia, &q).await.is_err() {
                            break;
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    // O comentário que aqui estava dizia "o próximo sync recupera", e isso
                    // era verdade quando por aqui só passavam mensagens de controlo. Já não
                    // é: passam também frames de ecrã e de câmara, e ESSES não se
                    // recuperam — não há sync que os vá buscar, porque um frame que se
                    // perdeu já não interessa a ninguém.
                    //
                    // Cair também não serve: derrubar a ligação porque um espectador se
                    // atrasou é trocar um soluço na imagem por uma chamada perdida. O que
                    // se faz é o que se pode fazer — seguir, e DIZER, para quem estiver a
                    // ler os registos saber que a imagem partida daquele momento tem uma
                    // explicação e não é um mistério.
                    eprintln!("[rede] {peer} atrasou-se e perdeu {n} pedaços");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
        }
    }
    // O `abort` e a remoção do mapa são agora do `SessaoViva`, que corre ao sair daqui
    // por qualquer porta. O `leitor` fica de fora do guarda porque o `select!` precisa
    // dele emprestado; aborta-se aqui, que é o único sítio onde ele já existe.
    leitor.abort();
    // Só se apaga do mapa se a ligação que lá está for ESTA. Ver e apagar têm de ser o
    // mesmo gesto: se entretanto entrou outra sessão, apagá-la deixava o par a parecer
    // desligado com uma ligação viva por baixo.
    Ok(())
}

/// Junta entradas recebidas ao servidor certo e avisa a interface se algo mudou.
/// Se este par conta como participante deste servidor.
///
/// É o critério que decide o que sai daqui para ele. Antes não havia critério nenhum: o
/// histórico de TODOS os servidores ia para QUALQUER par ligado, e cada mensagem escrita
/// era difundida a toda a gente com o id do servidor à frente. O conteúdo ia cifrado, mas
/// os ids, os autores e as horas iam em claro — ou seja, qualquer conhecido ficava a saber
/// em que salas eu estou, com quem, e quando falo.
/// Se este par partilha ALGUMA sala comigo.
///
/// É o porteiro. Até aqui não havia nenhum: `accept()` aceitava qualquer ligação com o ALPN
/// certo, e a partir daí um estranho com o meu `EndpointId` — que viaja em cada convite que
/// eu criei — podia injectar som nas minhas colunas a meio de uma chamada e, forjando uma
/// presença, passar a receber o meu microfone e a minha câmara. A cifra protegia o conteúdo
/// dos logs; o transporte não tinha porteiro nenhum.
/// A minha própria chave, que nunca deve entrar na lista de pares.
fn peer_proprio(app: &Arc<App>) -> String {
    app.minha_chave()
}

fn conhecido(app: &Arc<App>, peer: &str) -> bool {
    // A AMIZADE NÃO ENTRA AQUI, e foi uma decisão tomada depois de a escrever.
    //
    // Pus `if e_amigo(peer) { return true }` no princípio desta função, com o argumento de
    // que um amigo é alguém que eu escolhi. O argumento é bom e a consequência não: este
    // porteiro decide quem me pode pôr som nas colunas em QUALQUER chamada, incluindo uma
    // com colegas de um servidor onde o amigo não entra. Um datagrama de voz não leva sala
    // nenhuma — é só uma chave e bytes.
    //
    // «Estou disposto a ligar-me a ti» não é «podes interromper qualquer conversa minha».
    // E não se perde nada declarado: a amizade continua a dar ligação (é o vigia que disca)
    // e conversa privada (que vai pelo `participa`, não por aqui). Voz fora de um servidor
    // ainda não existe — quando existir, será com o consentimento de quem atende.
    app.servidores
        .lock()
        .map(|s| {
            s.values()
                // Uma CONVERSA não conta. Ela é guardada como um servidor — é isso que a
                // torna barata — mas não é uma sala partilhada: qualquer pessoa que tenha a
                // minha chave pública abre uma comigo, e a minha chave é pública por
                // desenho.
                //
                // Se contasse, a funcionalidade desfazia o porteiro construído para a
                // proteger: bastava abrir-me uma conversa para ganhar o direito de me pôr
                // som nas colunas e abrir descodificadores na minha máquina.
                //
                // O critério é partilhar uma SALA, e isso prova-se com a chave dela.
                .filter(|srv| srv.com.is_none())
                .any(|srv| srv.peers.iter().any(|p| p == peer))
        })
        .unwrap_or(false)
}

/// Aprende, do próprio log, que este par é de casa.
///
/// `srv.peers` só cresce com quem sincroniza connosco. Duas pessoas que entraram pelo mesmo
/// convite nunca se conhecem por aí — cada uma só tem o anfitrião — e sem isto ficariam a
/// falar através dele para sempre, e a recusar-se voz uma à outra. O `author` de cada
/// entrada está em claro: quem escreveu numa sala minha é, por definição, dessa sala.
fn aprender_dos_logs(app: &Arc<App>, peer: &str) {
    let aprendi = {
        let Ok(mut s) = app.servidores.lock() else {
            return;
        };
        let mut aprendi = false;
        for srv in s.values_mut() {
            // `escreveu` NÃO chega, e foi o meu engano: ele lê o campo `author`, que está
            // em claro, e o `merge` só verifica a assinatura. Ou seja, qualquer pessoa
            // anexa uma entrada de lixo assinada por si própria, volta a ligar-se, e este
            // caminho punha-a nos `peers` — exactamente o que eu tinha acabado de fechar no
            // `aplicar`. Fechei uma porta e deixei a gémea aberta ao lado.
            //
            // A prova é a mesma nos dois sítios: uma entrada que DECIFRA, o que exige a
            // chave da sala.
            if srv.autores_provados().contains(peer) && !srv.peers.iter().any(|p| p == peer) {
                srv.peers.push(peer.to_string());
                aprendi = true;
            }
        }
        aprendi
    };
    if aprendi {
        // #156: NÃO engolir o erro. Se a escrita falhar, o par aprendido fica só em memória
        // e volta a ser um estranho no arranque seguinte — sem porteiro, sem presença, e sem
        // uma linha em lado nenhum a explicar porquê. O `eprintln` já vai para o bruma.log.
        if let Err(e) = app.gravar_indice() {
            eprintln!("[dados] não consegui gravar o índice ao aprender um par: {e}");
        }
    }
}

/// Pode SINCRONIZAR aquele servidor comigo — que não é o mesmo que pertencer.
///
/// Distinção que custou uma porta aberta: quem me deu o convite tem de poder trocar comigo o
/// histórico daquela sala (senão entrar num servidor não funciona), mas NÃO tem de poder
/// pôr-me som nas colunas nem forjar presença só por eu ter aceitado um código que ele
/// escreveu. Trocar histórico de uma sala cuja chave ele obviamente tem não lhe dá nada que
/// ele já não tivesse; ser tratado como membro dá.
///
/// O porteiro (`conhecido`, `participa`) continua a ler só `peers`, onde só se entra
/// provando. Este é o único sítio onde o `convidou` conta.
/// As bandeiras de ataque existem, e NÃO existem no binário que vai para o utilizador.
///
/// Cinco delas fazem um guarda ser saltado ou escrevem estado permanente: o `BRUMA_ESTRANHO`
/// desliga o `pode_sincronizar` à saída, o simulador escreve lixo assinado num servidor REAL
/// escolhido às cegas, o `BRUMA_BLOQUEIA` e o `BRUMA_AMIGO` gravam no índice, e o
/// `BRUMA_BLOQUEIA_TARDE` derruba uma ligação.
///
/// Tudo isso é necessário para medir — um ataque que não sai de casa passa em todos os
/// testes — e nada disso tem que ver com usar a app. Ficando atrás de `debug_assertions`, o
/// binário de release simplesmente não os tem: não é «difícil de accionar», é inexistente.
/// Os testes correm sobre `target/debug` e continuam a tê-los todos.
#[cfg(debug_assertions)]
fn ataque_permitido() -> bool {
    std::env::var("BRUMA_ESTRANHO").is_ok()
}

/// Na release nem sequer se vai perguntar ao ambiente — e o nome da variável não fica no exe.
#[cfg(not(debug_assertions))]
fn ataque_permitido() -> bool {
    false
}

fn pode_sincronizar(app: &Arc<App>, servidor: &str, peer: &str) -> bool {
    app.servidores
        .lock()
        .map(|s| {
            s.get(servidor).is_some_and(|srv| {
                srv.peers.iter().any(|p| p == peer) || srv.convidou.as_deref() == Some(peer)
            })
        })
        .unwrap_or(false)
}

fn participa(app: &Arc<App>, servidor: &str, peer: &str) -> bool {
    app.servidores
        .lock()
        .map(|s| {
            s.get(servidor)
                .is_some_and(|srv| srv.peers.iter().any(|p| p == peer))
        })
        .unwrap_or(false)
}

fn aplicar(
    app: &Arc<App>,
    janela: &AppHandle,
    servidor: &str,
    entradas: Vec<blog::Entry>,
    peer: &str,
    rede: &Arc<Rede>,
) {
    // «Não temos este servidor» tem duas leituras, e só uma delas é «não é para nós».
    //
    // A outra é: ele acabou de abrir a nossa conversa e escreveu-me. Aí eu ainda não tenho
    // o log, e deitar fora era perder a primeira mensagem — e todas as seguintes, para
    // sempre, sem um erro em lado nenhum.
    //
    // O id distingue os dois casos sozinho, sem precisar de acreditar em ninguém: ele sai
    // das DUAS chaves públicas, portanto só um `Sync` vindo deste par pode trazer o id da
    // conversa deste par. Um estranho pode calcular o id dele comigo — e o que isso
    // significa é «alguém quis falar comigo», que é para o que serve.
    if !app.servidores.lock().unwrap().contains_key(servidor) {
        let minha = app.minha_chave();
        let nosso = match (crate::estado::hex32(&minha), crate::estado::hex32(peer)) {
            (Ok(a), Ok(b)) => Some(HEXLOWER.encode(&spike_common::crypto::id_da_conversa(&a, &b))),
            _ => None,
        };
        if nosso.as_deref() != Some(servidor) {
            return;
        }
        // E ele tem de poder escrever-me. É aqui que a política vive: uma conversa nova é
        // exactamente o momento em que alguém que eu não conheço me fala pela primeira vez.
        if !app.pode_escrever_me(peer) {
            eprintln!(
                "[porteiro] {} quis abrir uma conversa e a tua definição não deixa",
                &peer[..8.min(peer.len())]
            );
            return;
        }
        if let Err(e) = app.abrir_conversa(peer) {
            eprintln!(
                "[rede] {} quis falar comigo e não consegui abrir a conversa: {e}",
                &peer[..8.min(peer.len())]
            );
            return;
        }
        eprintln!(
            "[rede] {} abriu uma conversa comigo",
            &peer[..8.min(peer.len())]
        );
    }

    let (novas, aprendi) = {
        let mut s = app.servidores.lock().unwrap();
        let Some(srv) = s.get_mut(servidor) else {
            return; // não temos este servidor: não é erro, é só não ser para nós
        };

        // UMA CONVERSA TEM DOIS PARTICIPANTES, E SÓ DOIS.
        //
        // O id de uma conversa sai de duas chaves PÚBLICAS — qualquer pessoa que veja os dois
        // na lista de membros de um servidor o consegue calcular, sem falar com ninguém. Se
        // dizê-lo bastasse para entrar, um terceiro entrava na conversa dos outros: recebia o
        // log de volta (autores e horas em claro), ficava a saber que eles falam e quando, e
        // passava a contar como conhecido para a voz e para o vídeo.
        if srv.com.as_deref().is_some_and(|c| c != peer) {
            return;
        }

        // `merge_verificado`, e nao `log.merge`: o log so guarda o que DECIFRA.
        //
        // Isto fecha, na origem, a familia inteira de que ja tinha apanhado dois membros. O
        // `log.merge` verifica a assinatura -- que e feita com a chave de quem escreve, logo
        // qualquer pessoa assina o que quiser. Um estranho anexava-me lixo, sem limite de
        // tamanho, e ficava a constar como autor de uma sala onde nunca entrou.
        // #9: o merge devolve erro honesto quando a gravação falha (o log passou a gravar
        // antes de inserir). Engoli-lo com `unwrap_or(0)` era a pior falha que este projecto
        // reconhece: as mensagens apareciam no ecrã e desapareciam ao fechar. Regista-se e
        // avisa-se a interface.
        let novas = match srv.merge_verificado(entradas) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("[dados] não consegui gravar as mensagens que chegaram: {e}");
                let _ = janela.emit(
                    "erro-dados",
                    "Não consegui gravar as mensagens que chegaram. Verifica o espaço em disco —                      o que se passar a partir de agora pode perder-se.",
                );
                0
            }
        };

        // E aqui está o que faltava: PROVAR, e não dizer.
        //
        // Isto era `aprendi = !peers.contains(peer)` — ou seja, nomear um id que eu tenho era
        // prova bastante. Uma mensagem vazia punha um estranho nos meus peers, para sempre, e
        // a partir daí ele passava o porteiro da voz, do vídeo e da presença. Construí a porta
        // e deixei o caixilho de fora.
        //
        // A prova é ter escrito aqui uma entrada que DECIFRA, o que exige a chave da sala. E
        // de caminho aprendem-se todos os que a têm — que é o que faz duas pessoas entradas
        // pelo mesmo convite chegarem a conhecer-se, em vez de falarem para sempre através de
        // quem as convidou.
        let mut aprendi = false;
        if novas > 0 {
            let novos: Vec<String> = srv
                .autores_provados()
                .iter()
                .filter(|a| a.as_str() != peer_proprio(app) && !srv.peers.iter().any(|p| p == *a))
                .cloned()
                .collect();
            for autor in novos {
                if autor == peer {
                    aprendi = true;
                }
                srv.peers.push(autor);
            }
        }
        (novas, aprendi)
    };
    // Gravar SÓ quando se aprendeu um par (#140).
    //
    // O índice guarda chaves de servidores, peers, prekeys, amigos, bloqueados e marcas de
    // leitura — NÃO guarda mensagens. Quando só `novas > 0`, nada disso mudou: as mensagens
    // foram para o log (o ficheiro do servidor), e gravar o índice era serializar tudo,
    // cifrar tudo, escrever um temporário, `sync_data` e `rename` por uma mensagem que não lhe
    // diz respeito. Numa conversa activa era um fsync por mensagem recebida, à cadência que o
    // outro lado escolhia. O par que sincroniza sem trazer nada mas que se aprende cai em
    // `aprendi`, portanto continua a gravar; uma conversa nova grava-se no seu próprio ramo.
    if aprendi {
        if let Err(e) = app.gravar_indice() {
            eprintln!("[dados] não consegui gravar o índice: {e}");
            let _ = janela.emit(
                "erro-dados",
                "Não consigo escrever na pasta de dados. O que se passar a partir de agora                  pode perder-se.",
            );
        }
    }
    if novas > 0 {
        let _ = janela.emit("servidor-mudou", servidor);
    }
    // Ele deu-se a conhecer neste servidor: agora é participante, e recebe o que eu tenho.
    // É esta resposta — e não um sync voluntariado a toda a gente — que traz o histórico a
    // quem acabou de entrar por convite.
    if aprendi {
        let _ = rede.tx.send(Saida::SyncPara {
            para: peer.to_string(),
            servidor: servidor.to_string(),
        });

        // E dizer-lhe outra vez onde estou.
        //
        // A presença é dita UMA vez, no arranque da sessão — e nessa altura ele ainda não era
        // conhecido, portanto o guarda calou-a e ela nunca mais seria dita. O outro lado
        // ficava a ouvir-me sem me ver na lista: «0 presentes» com a voz a chegar.
        //
        // Agora que ele provou ser de casa, repete-se. Repetir uma presença não custa nada;
        // perdê-la custa a chamada inteira.
        let onde = rede.presenca.lock().ok().and_then(|p| p.clone());
        if let Some((srv_voz, canal)) = onde {
            if canal.is_some() {
                let _ = rede.tx.send(Saida::Presenca(srv_voz, canal));
            }
        }
    }
}

/// O que atravessa o fio.
///
/// Ate agora era so JSON, e para controlo continua a ser: e legivel, evolui sem partir
/// nada, e o volume e ridiculo. Video em JSON e que nao -- um array de numeros por cada
/// byte custaria varias vezes o proprio video. Por isso o quadro passa a dizer o que traz:
///
/// ```text
/// u32 tamanho | u8 tipo | corpo
///   tipo 0 -> Msg em JSON
///   tipo 1 -> video: u16 tamanho do cabecalho | cabecalho JSON | bytes crus
/// ```
///
/// O cabecalho do video vai em JSON na mesma, porque e pequeno e diz a que servidor e
/// canal pertence; o que fica cru sao os bytes que pesam.
pub enum Quadro {
    Controlo(Msg),
    Video {
        tipo: String,
        servidor: String,
        canal: String,
        dados: Vec<u8>,
    },
}

#[derive(Serialize, Deserialize)]
struct CabecalhoVideo {
    servidor: String,
    canal: String,
    /// `serde(default)` de propósito: uma versão anterior não escreve este campo, e sem o
    /// `default` a mensagem dela deixaria de ser entendida. Quem envia sem dizer o que é,
    /// está a enviar ecrã — era a única coisa que existia.
    #[serde(default = "ecra")]
    tipo: String,
}

fn ecra() -> String {
    "ecra".into()
}

const TIPO_CONTROLO: u8 = 0;
const TIPO_VIDEO: u8 = 1;

async fn escrever(envia: &mut SendStream, m: &Msg) -> Result<()> {
    escrever_quadro(envia, &Quadro::Controlo(m.clone())).await
}

async fn escrever_quadro(envia: &mut SendStream, q: &Quadro) -> Result<()> {
    let corpo: Vec<u8> = match q {
        Quadro::Controlo(m) => {
            let mut v = vec![TIPO_CONTROLO];
            v.extend_from_slice(&serde_json::to_vec(m)?);
            v
        }
        Quadro::Video {
            tipo,
            servidor,
            canal,
            dados,
        } => {
            let cab = serde_json::to_vec(&CabecalhoVideo {
                servidor: servidor.clone(),
                canal: canal.clone(),
                tipo: tipo.clone(),
            })?;
            let mut v = Vec::with_capacity(3 + cab.len() + dados.len());
            v.push(TIPO_VIDEO);
            v.extend_from_slice(&(cab.len() as u16).to_be_bytes());
            v.extend_from_slice(&cab);
            v.extend_from_slice(dados);
            v
        }
    };
    envia
        .write_all(&(corpo.len() as u32).to_be_bytes())
        .await
        .map_err(|e| anyhow!("write: {e}"))?;
    envia
        .write_all(&corpo)
        .await
        .map_err(|e| anyhow!("write: {e}"))?;
    Ok(())
}

async fn ler(recebe: &mut RecvStream) -> Result<Quadro> {
    let mut tam = [0u8; 4];
    recebe
        .read_exact(&mut tam)
        .await
        .map_err(|e| anyhow!("read: {e}"))?;
    let n = u32::from_be_bytes(tam) as usize;
    if n > MAX_FRAME {
        bail!("quadro de {n} bytes excede o limite");
    }
    if n == 0 {
        bail!("quadro vazio");
    }
    let mut corpo = vec![0u8; n];
    recebe
        .read_exact(&mut corpo)
        .await
        .map_err(|e| anyhow!("read: {e}"))?;

    match corpo[0] {
        TIPO_CONTROLO => Ok(Quadro::Controlo(serde_json::from_slice(&corpo[1..])?)),
        TIPO_VIDEO => {
            if corpo.len() < 3 {
                bail!("quadro de video truncado");
            }
            let tam_cab = u16::from_be_bytes([corpo[1], corpo[2]]) as usize;
            let fim = 3 + tam_cab;
            if corpo.len() < fim {
                bail!("cabecalho de video truncado");
            }
            let cab: CabecalhoVideo = serde_json::from_slice(&corpo[3..fim])?;
            Ok(Quadro::Video {
                tipo: cab.tipo,
                servidor: cab.servidor,
                canal: cab.canal,
                dados: corpo[fim..].to_vec(),
            })
        }
        outro => bail!("tipo de quadro desconhecido: {outro}"),
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    /// Uma mensagem de uma versao mais nova NAO pode derrubar a ligacao.
    ///
    /// Sem `#[serde(other)]`, o `serde` recusa um `t` que nao conheca, o `ler()` devolve
    /// `Err` e o leitor faz `break`. No dia em que uma versao acrescentasse uma mensagem,
    /// deixava de conseguir falar com todas as anteriores -- e o sintoma nao seria "essa
    /// funcionalidade nao funciona", seria "o outro aparece ligado e nao chega nada".
    #[test]
    fn uma_mensagem_desconhecida_nao_derruba_a_sessao() {
        // O que uma versao futura mandaria, com campos que esta nem imagina.
        let futuro = br#"{"t":"Conversa","id":"abc","entrada":{"seja_o_que_for":1}}"#;
        let m: Msg = serde_json::from_slice(futuro)
            .expect("uma mensagem desconhecida tem de desserializar, nao de falhar");
        assert!(
            matches!(m, Msg::Desconhecida),
            "devia cair na variante de recurso, deu {m:?}"
        );

        // E a tolerancia nao pode engolir o que se conhece: um `other` mal posto faz TUDO
        // cair na variante de recurso, e ai a app fica muda sem um unico erro.
        let ola: Msg = serde_json::from_slice(br#"{"t":"Ola","nome":"Rakjsu"}"#).unwrap();
        assert!(
            matches!(ola, Msg::Ola { .. }),
            "o Ola deixou de ser lido: {ola:?}"
        );
        let sync: Msg =
            serde_json::from_slice(br#"{"t":"Sync","servidor":"s","entradas":[]}"#).unwrap();
        assert!(
            matches!(sync, Msg::Sync { .. }),
            "o Sync deixou de ser lido: {sync:?}"
        );
    }

    /// A voz vai em datagramas, e datagramas não são streams: se o iroh não os suportar,
    /// ou se o par não os aceitar, o `send_datagram` falha em silêncio e a chamada fica
    /// muda sem um único erro. Isto prova que atravessam mesmo.
    ///
    /// Usa-se o preset sem relay e liga-se pelo endereço direto: o que está em causa é o
    /// transporte, não a descoberta, e depender de servidores externos tornava um teste
    /// de correção num teste de rede.
    /// A avaria que isto trava: `ligar` consultava só o mapa das ligações, e esse mapa só
    /// é escrito DEPOIS da ligação estar feita. Duas chamadas seguidas passavam ambas, e
    /// ficavam duas sessões para o mesmo par — com tudo a sair a dobrar, incluindo cada
    /// fragmento de ecrã.
    #[test]
    fn nao_se_liga_duas_vezes_ao_mesmo() {
        let ligacoes = Default::default();
        let a_ligar = Default::default();
        assert!(
            reservar(&ligacoes, &a_ligar, "abc"),
            "a primeira tentativa tem de passar"
        );
        assert!(
            !reservar(&ligacoes, &a_ligar, "abc"),
            "a segunda tem de ser recusada ENQUANTO a primeira ainda vai a caminho"
        );
        assert!(
            reservar(&ligacoes, &a_ligar, "xyz"),
            "outro par não é afectado"
        );
        // Falhou a ligação: a reserva morre com ela, senão a falha era permanente.
        a_ligar.lock().unwrap().remove("abc");
        assert!(
            reservar(&ligacoes, &a_ligar, "abc"),
            "depois de falhar, tem de se poder tentar outra vez"
        );
    }

    /// O que se prova aqui não é a regra em si, é que as DUAS máquinas chegam à mesma
    /// conclusão. Uma regra de desempate em que cada lado fica com a sua ligação deixa os
    /// dois a escrever para ninguém.
    #[test]
    fn o_desempate_da_o_mesmo_dos_dois_lados() {
        for (a, b) in [("aaa", "bbb"), ("bbb", "aaa"), ("m", "mm"), ("z1", "z2")] {
            // A ligação que A iniciou: A vê-a como iniciada, B vê-a como recebida.
            let a_sobre_a_dela = fica_esta(a, b, true);
            let b_sobre_a_dele = fica_esta(b, a, false);
            assert_eq!(
                a_sobre_a_dela, b_sobre_a_dele,
                "{a} e {b} discordam sobre a ligação que {a} iniciou"
            );
            // E a outra ligação, a que B iniciou, tem de ter o veredicto CONTRÁRIO —
            // senão ou sobreviviam as duas, ou não sobrevivia nenhuma.
            let a_sobre_a_dele = fica_esta(a, b, false);
            assert_ne!(
                a_sobre_a_dela, a_sobre_a_dele,
                "{a} não pode querer ficar com as duas ligações a {b}"
            );
        }
    }

    #[tokio::test]
    async fn um_datagrama_atravessa_a_ligacao() {
        let a = Endpoint::builder(presets::N0DisableRelay)
            .alpns(vec![ALPN.to_vec()])
            .bind()
            .await
            .expect("endpoint A");
        let b = Endpoint::builder(presets::N0DisableRelay)
            .alpns(vec![ALPN.to_vec()])
            .bind()
            .await
            .expect("endpoint B");

        let endereco_b = b.addr();
        let ouvinte = tokio::spawn(async move {
            let incoming = b.accept().await.expect("ligação a chegar");
            let conn = incoming.await.expect("aceitar");
            conn.read_datagram().await.expect("datagrama")
        });

        let conn = a.connect(endereco_b, ALPN).await.expect("ligar");
        assert!(
            conn.max_datagram_size().is_some(),
            "o par tem de aceitar datagramas, senão a voz não sai daqui"
        );
        conn.send_datagram(bytes::Bytes::from_static("ola voz".as_bytes()))
            .expect("enviar");

        let recebido = tokio::time::timeout(std::time::Duration::from_secs(10), ouvinte)
            .await
            .expect("não chegou a tempo")
            .expect("tarefa");
        assert_eq!(&recebido[..], b"ola voz");
    }
}
