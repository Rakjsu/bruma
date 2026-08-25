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
}

/// O que sai daqui para as sessoes abertas.
#[derive(Clone, Debug)]
pub enum Saida {
    Entrada(String, blog::Entry),
    Presenca(String, Option<String>),
    /// Dirigido a UM peer. As outras sessoes ignoram.
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
            v.sort();
            v.dedup();
            v
        };
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

    let _ = janela.emit("peer-ligado", &peer);

    // A voz chega por aqui, fora do stream de controlo: um datagrama por pedaço de som.
    let voz_conn = conn.clone();
    let voz_peer = peer.clone();
    let contagem = rede.clone();
    let ouvinte = tokio::spawn(async move {
        loop {
            let d = match voz_conn.read_datagram().await {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("[rede] fim dos datagramas de {voz_peer}: {e}");
                    break;
                }
            };
            if let Ok(mut n) = contagem.contagem.lock() {
                n.entry(voz_peer.clone()).or_default().voz_rec += 1;
            }
            crate::comandos::voz_recebida(&voz_peer, &d);
        }
    });

    let meu_nome = app.nome.lock().unwrap().clone();
    escrever(&mut envia, &Msg::Ola { nome: meu_nome }).await?;

    // Manda tudo o que temos. Quem não tiver o servidor ignora — é mais simples e mais
    // robusto do que negociar primeiro quem tem o quê.
    let pacotes: Vec<(String, Vec<blog::Entry>)> = {
        let s = app.servidores.lock().unwrap();
        s.values()
            .map(|srv| (srv.id.clone(), srv.log.ordered()))
            .collect()
    };
    for (servidor, entradas) in pacotes {
        escrever(&mut envia, &Msg::Sync { servidor, entradas }).await?;
    }

    // E dizer onde estamos agora. Sem isto, quem chega depois de nós entrarmos numa sala
    // não sabe que lá estamos, e não nos manda voz nenhuma.
    let onde = rede.presenca.lock().ok().and_then(|p| p.clone());
    if let Some((servidor, canal)) = onde {
        if canal.is_some() {
            escrever(&mut envia, &Msg::Presenca { servidor, canal }).await?;
        }
    }

    let rede_leitura = rede.clone();
    let leitura_app = app.clone();
    let leitura_janela = janela.clone();
    let peer_leitura = peer.clone();
    let mut leitor = tokio::spawn(async move {
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
                    if let Ok(mut n) = rede_leitura.contagem.lock() {
                        n.entry(peer_leitura.clone()).or_default().ecra_rec += 1;
                    }
                    if tipo == "camara" {
                        crate::comandos::camara_recebida(&peer_leitura, dados);
                    } else {
                        crate::comandos::ecra_recebido(&peer_leitura, &servidor, &canal, dados);
                    }
                }
                Ok(Quadro::Controlo(Msg::Ola { nome })) => {
                    let _ = leitura_janela.emit("peer-nome", (&peer_leitura, &nome));
                }
                Ok(Quadro::Controlo(Msg::Sync { servidor, entradas })) => {
                    aplicar(
                        &leitura_app,
                        &leitura_janela,
                        &servidor,
                        entradas,
                        &peer_leitura,
                    );
                }
                Ok(Quadro::Controlo(Msg::Nova { servidor, entrada })) => {
                    aplicar(
                        &leitura_app,
                        &leitura_janela,
                        &servidor,
                        vec![entrada],
                        &peer_leitura,
                    );
                }
                Ok(Quadro::Controlo(Msg::Presenca { servidor, canal })) => {
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
                    let _ = leitura_janela.emit(
                        "sinal",
                        serde_json::json!({ "de": &peer_leitura, "servidor": servidor, "canal": canal, "dados": dados }),
                    );
                }
                Err(_) => break,
            }
        }
    });

    let mut sub = rede.tx.subscribe();
    loop {
        tokio::select! {
            // Só o leitor de controlo decide o fim. O de datagramas acabar não é motivo
            // para fechar nada: se ele parar, perde-se a voz — não o servidor inteiro. Ter
            // os dois aqui fazia com que qualquer soluço na voz levasse o chat com ele.
            _ = &mut leitor => break,
            got = sub.recv() => match got {
                Ok(saida) => {
                    let quadro = match saida {
                        Saida::Entrada(servidor, entrada) => {
                            Some(Quadro::Controlo(Msg::Nova { servidor, entrada }))
                        }
                        Saida::Presenca(servidor, canal) => {
                            Some(Quadro::Controlo(Msg::Presenca { servidor, canal }))
                        }
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
    leitor.abort();
    ouvinte.abort();
    // Só se apaga do mapa se a ligação que lá está for ESTA. Ver e apagar têm de ser o
    // mesmo gesto: se entretanto entrou outra sessão, apagá-la deixava o par a parecer
    // desligado com uma ligação viva por baixo.
    let era_a_nossa = rede
        .ligacoes
        .lock()
        .map(|mut l| {
            if l.get(&peer).map(|(_, s)| *s) == Some(serie) {
                l.remove(&peer);
                true
            } else {
                false
            }
        })
        .unwrap_or(false);
    if era_a_nossa {
        let _ = janela.emit("peer-desligado", &peer);
    }
    Ok(())
}

/// Junta entradas recebidas ao servidor certo e avisa a interface se algo mudou.
fn aplicar(
    app: &Arc<App>,
    janela: &AppHandle,
    servidor: &str,
    entradas: Vec<blog::Entry>,
    peer: &str,
) {
    let novas = {
        let mut s = app.servidores.lock().unwrap();
        let Some(srv) = s.get_mut(servidor) else {
            return; // não temos este servidor: não é erro, é só não ser para nós
        };
        if !srv.peers.iter().any(|p| p == peer) {
            srv.peers.push(peer.to_string());
        }
        srv.log.merge(entradas).unwrap_or(0)
    };
    if novas > 0 {
        let _ = app.gravar_indice();
        let _ = janela.emit("servidor-mudou", servidor);
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
