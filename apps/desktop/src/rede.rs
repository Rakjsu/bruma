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
use iroh::endpoint::{presets, Connection, QuicTransportConfig, RecvStream, SendStream};
use iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey, Watcher};
use serde::{Deserialize, Serialize};
use spike_common::log as blog;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::sync::broadcast;

use crate::estado::App;

pub const ALPN: &[u8] = b"bruma/1";
const MAX_FRAME: usize = 8 * 1024 * 1024;

/// Quanto vai, no máximo, num `Sync` — e porque é MUITO menor do que o `MAX_FRAME`.
///
/// O `Sync` levava o log INTEIRO de uma sala num só quadro. Acima dos 8 MiB do `MAX_FRAME` o
/// outro lado recusa-o pelo cabeçalho, **antes de ler o corpo**: o leitor sai, a sessão morre,
/// e como o histórico não encolhe sozinho a tentativa seguinte manda exactamente o mesmo. Uma
/// sala grande passava a nunca poder sincronizar, com o sintoma «aparece ligado e não chega
/// nada». Bastavam umas 14 mil mensagens, ou 260 no limite dos 4000 caracteres.
///
/// 256 KiB, e não «um pouco menos de 8 MiB», por causa do PRAZO. Cada quadro tem o seu prazo
/// de escrita (generoso no controlo, mas finito), e o prazo é POR QUADRO: num quadro único, um
/// par com pouca largura de banda esgota-o e a sessão cai; em lotes, cada um cabe à vontade e
/// a sincronização **progride** em vez de reiniciar do zero. É a mesma lição do teste do
/// impasse — medir progresso, não tempo decorrido.
const LOTE_SYNC: usize = 256 * 1024;

/// Quanto tempo um par tem para dizer `Ola` antes de a ligação ser fechada (#195).
///
/// O ciclo de `accept` aceitava qualquer ligação com o ALPN certo e lançava uma tarefa por
/// cada uma. Uma ligação que nunca fala fica de pé para sempre: come uma entrada no mapa, uma
/// tarefa, e uma linha no contador de «ligados» da interface — sem nunca ter dito quem é.
///
/// Trinta segundos e não três: entre o Brasil e os EUA, com o aperto de mão do QUIC e um
/// arranque de app pelo meio, um par legítimo pode demorar. O que isto corta é quem NUNCA
/// fala, não quem fala devagar.
const PRAZO_DO_OLA: std::time::Duration = std::time::Duration::from_secs(30);

/// Quantos pares que ainda não provaram nada podem estar ligados ao mesmo tempo (#195).
///
/// Um par «de casa» — que partilha uma sala comigo, ou que eu convidei — nunca conta para
/// este tecto: por muito cheia que a casa esteja, ela entra sempre. O tecto é só para quem
/// tem a minha chave (que viaja em qualquer convite reencaminhado) e ainda não provou nada.
///
/// Cinco chega de sobra para uma app de duas pessoas, e é generoso o bastante para o caso
/// legítimo que interessa: várias pessoas a entrar pelo mesmo convite ao mesmo tempo.
const TECTO_DE_ESTRANHOS: usize = 5;

/// Quantas entradas ILEGÍVEIS se aceitam de um par que ainda não provou pertencer à sala (#85).
///
/// # A avaria que isto fecha
///
/// À saída havia porteiro; à entrada não havia nenhum. O `Msg::Sync` e o `Msg::Nova` iam
/// direitos ao `aplicar`, e um estranho ligado podia mandar quadros cheios de entradas falsas
/// com o id de uma sala minha — e o id de uma sala viaja em claro em qualquer convite
/// reencaminhado. Cada entrada custa uma verificação de assinatura e uma tentativa de
/// decifragem, **tudo com o mutex dos servidores segurado**: a app inteira congela enquanto
/// isso corre, e repete-se a cada ligação.
///
/// O que se conta são as RECUSADAS, e não o total — e essa é a diferença entre uma defesa e
/// uma avaria. Um par legítimo com histórico grande manda milhares de entradas e todas
/// decifram: não gasta nada deste orçamento, por muito que mande. Um estranho gasta-o à
/// primeira, porque não tem a chave e nenhuma das dele decifra.
///
/// Cinquenta é folgado de propósito: um par legítimo pode trazer entradas de uma sala cuja
/// chave rodou, ou restos de um formato antigo, e isso não deve fechar-lhe a porta.
const LIXO_TOLERADO: usize = 50;

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
        /// A versão do Bruma do outro lado (#4).
        ///
        /// Vai da MESMA forma que os campos acima — `Option` com `default` numa variante que
        /// já existe — precisamente pela razão escrita ali: uma variante nova derrubaria a
        /// ligação com qualquer versão anterior.
        ///
        /// Serve para a degradação deixar de ser muda. Quando chega uma mensagem que esta
        /// versão não conhece, ignora-se e segue-se — o que é a decisão certa para a ligação
        /// e a errada para a pessoa: ela vê uma funcionalidade a não funcionar e não tem como
        /// saber que é porque o outro está noutra versão. Um par sem este campo é uma versão
        /// anterior a esta, o que também é informação.
        #[serde(default)]
        versao: Option<String>,
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
    /// «Mandei-te N pedaços de voz» — o que torna a perda MEDÍVEL (#124).
    ///
    /// # Porque é que isto vai pelo controlo e não dentro do datagrama
    ///
    /// O backlog pede um número de sequência à frente de cada datagrama de voz. Dá o mesmo
    /// número e mais algum (ordem, duplicados) — e parte o áudio de quem ainda não
    /// actualizou: um par na versão anterior lê os bytes do cabeçalho como se fossem Opus e
    /// faz BARULHO. Silêncio é mau; barulho na coluna de alguém é pior, e não há sítio no
    /// datagrama onde se possa pôr uma marca que o Opus não possa legitimamente ter.
    ///
    /// Isto dá a percentagem de perda — que era o objectivo — sem tocar no fio: quem manda
    /// diz, de dois em dois segundos, quantos pedaços já mandou naquela sala. Quem recebe
    /// compara com os que contou. A diferença é a perda, e passa a haver um número onde havia
    /// um palpite.
    ///
    /// E é seguro desde já: o `#[serde(other)] Desconhecida` existe desde a v0.18.0, portanto
    /// qualquer versão publicada a ignora sem cair. Não precisa de travessia nenhuma.
    ///
    /// O que ISTO não dá é a ordem: um pedaço que chegue fora de ordem continua a ser tocado
    /// fora de ordem. Essa metade precisa mesmo do cabeçalho, e do procedimento de duas
    /// versões que o `ESCREVER_PRESO_AO_AUTOR` já documenta para a cifra.
    Vozes {
        servidor: String,
        canal: String,
        enviados: u64,
    },
    /// «Não aceito conversas de quem não conheço» — dito, em vez de silêncio (#131).
    ///
    /// # A avaria que isto fecha
    ///
    /// Quando alguém me abria uma conversa e a minha política dizia que não, escrevia-se uma
    /// linha no registo DE QUEM RECUSA e não se respondia nada. Do outro lado, tudo tinha
    /// corrido bem: a conversa aparecia na interface, as mensagens ficavam no log local, e o
    /// envio saía sem erro. A pessoa escrevia durante dias para uma sala que só existia na
    /// máquina dela.
    ///
    /// É seguro acrescentar isto porque o `#[serde(other)] Desconhecida` já existe desde a
    /// v0.18.0: um par nessa versão ou mais recente ignora-a sem derrubar a sessão. Um par em
    /// v0.17 ou anterior derrubaria — e isso é aceitável porque este quadro só chega a quem
    /// tentou abrir-me uma conversa e foi recusado, que é precisamente o caminho que hoje
    /// acaba em nada.
    ///
    /// E não conta nada de novo a ninguém: que a minha chave está viva, quem se liga já sabe
    /// pela ligação ter pegado.
    Recusa { servidor: String },
    #[serde(other)]
    Desconhecida,
}

/// O que sai daqui para as sessoes abertas.
#[derive(Clone, Debug)]
pub enum Saida {
    Entrada(String, blog::Entry),
    Presenca(String, Option<String>),
    /// «Mandei-te N pedaços de voz» — dirigido a UM peer (#124).
    Vozes {
        para: String,
        servidor: String,
        canal: String,
        enviados: u64,
    },
    /// «Não te aceito» — dirigido a UM peer (#131).
    Recusa {
        para: String,
        servidor: String,
    },
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
    /// Quantos datagramas de voz o `send_datagram` RECUSOU (#34).
    ///
    /// Só se contava o que saía bem. O ramo do erro não existia — não contava, não registava,
    /// não avisava. Uma recusa ocasional é legítima (uma partilha de ecrã a 60
    /// fps ao lado enche-o), mas uma recusa PERMANENTE — um par cujo transporte não aceita
    /// datagramas de todo — dava exactamente a mesma coisa: silêncio, com a app a dizer «Voz
    /// conectada».
    ///
    /// Com este número ao lado do `voz_env`, os dois casos deixam de se confundir: um é uns
    /// poucos em milhares, o outro é todos.
    pub voz_falhados: u64,
    /// A escrita mais LENTA para este par, em ms, e a última (#114).
    ///
    /// Dentro do `select!` da sessão, o `escrever_quadro` fica à espera quando a janela de
    /// fluxo do outro lado enche. Enquanto espera, esta sessão **não está a ler do canal de
    /// difusão** — e é assim que se chega ao `Lagged` que deitava fora mensagens de texto
    /// (#53). Não havia nenhum número, em lado nenhum, que dissesse «a escrita para este
    /// par demorou três segundos».
    pub escrita_pior_ms: u64,
    pub escrita_ultima_ms: u64,
    /// Quando é que a `escrita_ultima_ms` foi medida.
    ///
    /// # O ferrolho que isto abre
    ///
    /// Sem este campo, o corte de vídeo era uma armadilha que se fechava sozinha e nunca mais
    /// abria: cortar um fragmento significa NÃO escrever, não escrever significa não medir, e
    /// não medir significa que a `escrita_ultima_ms` fica presa no valor alto que causou o
    /// corte. Uma única escrita de 501 ms — trivial num relay saturado — matava a partilha de
    /// ecrã para aquele par **para o resto da sessão**, e o `contagem` não é limpo na
    /// religação, portanto também para as sessões seguintes.
    ///
    /// Numa partilha de ecrã com o microfone silenciado o vídeo é o único tráfego contínuo,
    /// portanto não havia mais nada a passar por ali que voltasse a medir.
    pub escrita_medida_em: Option<std::time::Instant>,
    /// Quantos fragmentos de vídeo se deitaram fora por a escrita estar atrasada (#114).
    ///
    /// A imagem parte — e parte no PRESENTE, em vez de chegar atrasada e continuar a segurar
    /// o texto atrás dela. É uma decisão que se vê, e não uma que se adivinha.
    pub video_cortado: u64,
    /// Quando saiu e quando entrou o ÚLTIMO datagrama de voz, e o que os contadores diziam há
    /// um segundo (#33).
    ///
    /// # A avaria que isto torna visível
    ///
    /// Os contadores só crescem, desde que a ligação abriu. O painel só marcava «mudo» quando
    /// `recebidos == 0 && enviados > 0` — ou seja, só apanhava a avaria que existiu desde o
    /// primeiro segundo. Se a voz da outra pessoa morresse ao minuto dez, o painel mostrava
    /// «↑30000 ↓29000» e parecia saudável para sempre.
    ///
    /// Um total não sabe dizer «agora». Um instante sabe.
    pub ultimo_env: Option<std::time::Instant>,
    pub ultimo_rec: Option<std::time::Instant>,
    /// A fotografia de há um segundo, para se poder dizer o RITMO e não só o acumulado.
    marca: Option<(std::time::Instant, u64, u64)>,
    /// Pacotes por segundo, calculados na última vez que alguém perguntou.
    pub env_s: u64,
    pub rec_s: u64,
    /// Quantos pedaços de voz este par DIZ ter-me mandado (#124).
    ///
    /// Vem do `Msg::Vozes` dele. A diferença para o `voz_rec` é a perda — e é o único sítio
    /// de onde ela pode vir: o receptor, sozinho, não tem como distinguir «ele calou-se» de
    /// «perdi trinta pacotes».
    pub disse_ter_enviado: u64,
    /// Itens do canal de difusão que ESTA sessão saltou por se ter atrasado (#166): texto,
    /// presença, `Vozes`, `Sinal`, `Video` de todos os pares — e não «fragmentos de imagem
    /// deste par». O texto volta pelo `SyncPara`; a imagem não. Acumula desde que a app
    /// abriu. Ficava num `eprintln!` que ninguém abre durante uma chamada.
    pub perdidos_no_canal: u64,
    /// Frames de câmara enviados, à parte do ecrã (#133). O `ecra_env` contava os dois, e
    /// «sem espectadores não sai um byte de ecrã» (#71) não se conseguia medir com a câmara
    /// ligada.
    pub camara_env: u64,
    /// Frames de câmara cortados por atraso, à parte dos de ecrã (`video_cortado`).
    pub camara_cortada: u64,
    /// A fotografia da ligação de há um segundo, para os ritmos do caminho (#115): quando,
    /// bytes UDP enviados, pacotes perdidos.
    amostra_do_caminho: Option<(std::time::Instant, u64, u64)>,
    /// O que o caminho fez no último segundo medido (#115): kbit/s enviados e pacotes
    /// perdidos por segundo. `None` até haver duas fotografias — ausência de medida não é
    /// zero (#171).
    pub tx_kbps: Option<f64>,
    pub perdidos_s: Option<f64>,
}

impl Contagem {
    /// Actualiza os ritmos do caminho a partir das estatísticas da ligação (#115), se já
    /// passou um segundo desde a última fotografia. Recebe os números e não o tipo das
    /// `stats`: só se lê deles, nunca se constrói nenhum.
    pub fn recalcular_caminho(&mut self, agora: std::time::Instant, tx_bytes: u64, perdidos: u64) {
        match self.amostra_do_caminho {
            // Os contadores são da LIGAÇÃO e recomeçam em zero numa religação; a fotografia
            // é da `Contagem`, que sobrevive. Um total mais pequeno do que a fotografia é
            // uma ligação nova: recomeça-se a medir em vez de dizer «0 kbit/s» (revisão).
            Some((_, tx0, p0)) if tx_bytes < tx0 || perdidos < p0 => {
                self.amostra_do_caminho = Some((agora, tx_bytes, perdidos));
                self.tx_kbps = None;
                self.perdidos_s = None;
            }
            Some((quando, tx0, p0)) => {
                let dt = agora.duration_since(quando).as_secs_f64();
                if dt >= 1.0 {
                    self.tx_kbps = Some(tx_bytes.saturating_sub(tx0) as f64 * 8.0 / dt / 1000.0);
                    self.perdidos_s = Some(perdidos.saturating_sub(p0) as f64 / dt);
                    self.amostra_do_caminho = Some((agora, tx_bytes, perdidos));
                }
            }
            None => self.amostra_do_caminho = Some((agora, tx_bytes, perdidos)),
        }
    }

    /// Actualiza o ritmo, se já passou tempo suficiente desde a última vez.
    ///
    /// Corre quando a interface pergunta (uma vez por segundo), e não a cada datagrama: a cada
    /// datagrama seriam cinquenta divisões por segundo por par, num mutex que já é o mais
    /// disputado da app. Aqui é uma por pergunta.
    pub fn recalcular_ritmo(&mut self, agora: std::time::Instant) {
        match self.marca {
            Some((quando, env, rec)) => {
                let dt = agora.duration_since(quando).as_secs_f64();
                if dt >= 1.0 {
                    self.env_s = ((self.voz_env - env) as f64 / dt).round() as u64;
                    self.rec_s = ((self.voz_rec - rec) as f64 / dt).round() as u64;
                    self.marca = Some((agora, self.voz_env, self.voz_rec));
                }
            }
            None => self.marca = Some((agora, self.voz_env, self.voz_rec)),
        }
    }

    /// A perda, em percentagem, se houver com que a calcular (#124).
    ///
    /// `None` quando o outro lado ainda não disse nada — e isso NÃO é zero por cento. É a
    /// mesma distinção do RTT: a ausência de medida não se pinta de bom resultado.
    ///
    /// O valor é aparado a zero em baixo porque as duas contagens não são tiradas no mesmo
    /// instante: o anúncio dele viaja, e nesse tempo podem chegar mais pedaços. Uma perda
    /// «negativa» é o relógio, não a rede.
    pub fn perda_por_cento(&self) -> Option<f64> {
        if self.disse_ter_enviado == 0 {
            return None;
        }
        let perdidos = self.disse_ter_enviado.saturating_sub(self.voz_rec);
        Some((perdidos as f64 * 100.0 / self.disse_ter_enviado as f64).clamp(0.0, 100.0))
    }

    /// Há quantos milissegundos chegou o último datagrama de voz deste par, se algum chegou.
    pub fn ha_quanto_rec(&self, agora: std::time::Instant) -> Option<u64> {
        self.ultimo_rec
            .map(|t| agora.duration_since(t).as_millis() as u64)
    }
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
    /// Os ultimos acontecimentos de rede, com hora (#119). Ver `anotar_na_rede`.
    pub diario: std::sync::Mutex<Vec<(String, String)>>,
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
        // O fio dos porteiros para a interface (#139). Um envenenamento do mapa de
        // servidores põe todos eles a recusar; sem isto, recusariam em silêncio.
        let _ = JANELA.set(janela.clone());
        // A FILA DE SAÍDA DOS DATAGRAMAS: 1 MB por omissão, e isso são MINUTOS de voz (#173).
        //
        // O Bruma nunca chamou `transport_config`, portanto herdava o `datagram_send_buffer_size`
        // por omissão do QUIC: 1 MiB. A voz sai a 24 kbps de Opus mais 32 bytes de chave por
        // pedaço, uns 4,6 KB/s por par — 1 MiB é **mais de três minutos** de fala à espera de
        // sair. Quando a ligação engasga, o que enche essa fila não é dado que valha a pena
        // guardar: é voz que, quando chegasse, já não interessava a ninguém.
        //
        // A política do QUIC quando a fila enche é deitar fora os MAIS VELHOS, que é exactamente
        // a certa para voz — e é por isso que a correcção é só escolher o tamanho.
        //
        // # Porque 16 KiB, e o que este número NÃO é
        //
        // No fio vai só o Opus — a chave de quem falou é acrescentada na recepção, não no
        // envio (`enviar_voz` manda `dados` tal e qual). A 24 kbps são 3000 bytes por
        // segundo, portanto 16 KiB são uns **5,5 segundos** de fala, e não os 3,5 que aqui
        // estiveram escritos por eu ter contado um prefixo que não existe neste sentido.
        //
        // E 5,5 s continua muito acima do que a reprodução do outro lado consegue usar: a
        // folga vai de 80 a 200 ms, logo tudo o que esteja nesta fila há mais de meio segundo
        // já chega tarde. O valor não foi escolhido por esse critério — foi escolhido com
        // margem larga e MEDIDO: no teste de par a perda fica em 0,0%. Apertá-lo mais é
        // possível e não foi feito, porque não há medida que o justifique ainda.
        //
        // O instrumento para essa medida existe e passa a estar no painel: o
        // `datagram_send_buffer_space()` diz quanto espaço LIVRE resta nesta fila. Se ele
        // nunca descer perto de zero, o buffer nunca esteve sequer perto de encher.
        //
        // E há como saber se este número é pequeno de mais, sem adivinhar. Não é pelo
        // `voz_falhados`: uma fila cheia **não** faz o `send_datagram` devolver `Err` — ele
        // aceita e deita fora os velhos, em silêncio. Quem denuncia é a PERDA do outro lado
        // (#124), que compara os que eu digo ter mandado com os que ele contou. Se este valor
        // estivesse apertado de mais, a perda subiria no par — e ela mede-se lá.
        //
        // Isto não afecta o ecrã nem a câmara: só a voz vai por datagramas (`enviar_voz`); o
        // vídeo vai por `Saida::Video`, que são streams.
        let transporte = QuicTransportConfig::builder()
            .datagram_send_buffer_size(16 * 1024)
            .build();
        let endpoint = Endpoint::builder(presets::N0)
            .secret_key(SecretKey::from_bytes(&app.semente))
            .alpns(vec![ALPN.to_vec()])
            .transport_config(transporte)
            .bind()
            .await
            .map_err(|e| anyhow!("não consegui abrir a rede: {e}"))?;

        let (tx, _) = broadcast::channel(512);
        let rede = Arc::new(Rede {
            diario: std::sync::Mutex::new(Vec::new()),
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
    /// Falhas aqui são um datagrama grande de mais ou um par que não aceita datagramas —
    /// **não** uma fila cheia. Com a fila cheia o `send_datagram` devolve `Ok` e o QUIC
    /// deita fora os mais velhos, em silêncio; quem denuncia esse caso é a perda que o
    /// outro lado calcula (#124), e não este contador. Contam-se na mesma (#34): todas as
    /// recusas seguidas são um par que não recebe voz nenhuma.
    pub fn enviar_voz(&self, para: &[String], dados: &[u8]) {
        let Ok(ligacoes) = self.ligacoes.lock() else {
            return;
        };
        let bytes = bytes::Bytes::copy_from_slice(dados);
        let agora = std::time::Instant::now();
        for p in para {
            if let Some((c, _)) = ligacoes.get(p) {
                // O ramo do ERRO passa a existir (#34). Uma recusa ocasional é normal sob
                // carga; todas as recusas seguidas são um par que não recebe voz nenhuma, e
                // até aqui as duas davam o mesmo: silêncio.
                let saiu = c.send_datagram(bytes.clone()).is_ok();
                if let Ok(mut n) = self.contagem.lock() {
                    let e = n.entry(p.clone()).or_default();
                    if saiu {
                        e.voz_env += 1;
                        e.ultimo_env = Some(agora);
                    } else {
                        e.voz_falhados += 1;
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

/// O recuo da religação, por par — e sem rede nem relógio lá dentro.
///
/// Está à parte do vigia pela mesma razão que o `interpretar` está à parte do `ler`: é uma
/// DECISÃO, e uma decisão testa-se sozinha. O vigia precisa de um `Endpoint`, de sessões vivas
/// e de esperas reais; isto precisa de um `Instant` e de mais nada.
///
/// A regra que aqui está escrita, e que é a correcção do defeito: **só o [`Adiamento::pegou`]
/// limpa o recuo.** Discar não o limpa, nem sequer quando o outro lado atende — porque atender
/// não é funcionar, e o `ligar` devolve `Ok` mal faz `spawn` da sessão, antes de um único byte
/// ir ao fio.
#[derive(Default)]
struct Adiamento {
    espera: std::collections::HashMap<String, u64>,
    proxima: std::collections::HashMap<String, std::time::Instant>,
}

impl Adiamento {
    /// Há uma sessão viva com este par — e isto é PROVA e não promessa: ele está no mapa das
    /// ligações. O recuo volta a zero, para que uma queda a seguir a uma hora de conversa seja
    /// tentada dois segundos depois e não daqui a um minuto.
    ///
    /// # Porque é que isto não briga com o sync em lotes
    ///
    /// As duas correcções deste mesmo commit podiam morder-se: se a inscrição no mapa
    /// acontecesse só DEPOIS do aperto de mão e dos `Sync`, um sync grande — que agora demora
    /// mais voltas, por ir em lotes — mantinha o `ja_ligado` falso durante todo esse tempo, e
    /// o recuo crescia até aos 60 s por uma sessão que estava a funcionar perfeitamente.
    ///
    /// Não acontece: a `sessao` inscreve-se no mapa `ligacoes` logo à cabeça, antes de abrir o
    /// stream e muito antes do primeiro `Sync`. A janela entre o `ligar` devolver `Ok` e o
    /// `ja_ligado` passar a verdadeiro é o tempo de arrancar uma tarefa, não o tempo de
    /// sincronizar um histórico.
    fn pegou(&mut self, peer: &str) {
        self.espera.remove(peer);
        self.proxima.remove(peer);
    }

    /// Ainda não chegou a hora da próxima tentativa.
    fn ainda_cedo(&self, peer: &str, agora: std::time::Instant) -> bool {
        self.proxima.get(peer).is_some_and(|q| agora < *q)
    }

    /// Quanto tempo se espera quando a falha é NOSSA — sem relay de casa (#172).
    ///
    /// # Porque é que não é «o mesmo que da última vez»
    ///
    /// A primeira versão disto devolvia o valor guardado sem o fazer crescer. Só que o valor
    /// guardado de um par que nunca foi tentado é 2 — e sem relay de casa TODAS as tentativas
    /// falham. Resultado: discava-se de dois em dois segundos, para sempre, a um par que não
    /// pode responder. É exactamente o martelo que a documentação do `Adiamento` descreve,
    /// com a única diferença de a culpa ser nossa.
    ///
    /// Não punir o par continua certo — o recuo DELE não cresce. O que se acrescenta é um
    /// intervalo próprio para este caso, fixo e maior, porque sem relay tentar depressa não
    /// serve para nada.
    const SEM_RELAY_S: u64 = 10;

    fn sem_relay(&mut self, peer: &str, agora: std::time::Instant) -> u64 {
        self.proxima.insert(
            peer.to_string(),
            agora + std::time::Duration::from_secs(Self::SEM_RELAY_S),
        );
        Self::SEM_RELAY_S
    }

    /// A rede mudou por baixo de nós: o castigo acumulado deixa de fazer sentido (#55).
    ///
    /// # A avaria que isto fecha
    ///
    /// O recuo cresce de 2 até 60 segundos e nada o encurtava a não ser uma ligação que
    /// pegasse. Cenário do enunciado, e não é hipotético: o amigo passa do Wi-Fi para os
    /// dados do telemóvel a meio de uma conversa. As ligações antigas morrem, o vigia tenta,
    /// falha — porque o endereço mudou e o de cá ainda não sabe —, e a cada falha o recuo
    /// duplica. Ao fim de meia dúzia de tentativas ficam **sessenta segundos** de silêncio à
    /// espera de uma religação que já podia ter acontecido.
    ///
    /// O que o encurta é o único facto que diz «tudo o que aprendi sobre alcançar esta gente
    /// deixou de valer»: a máquina mudou de rede.
    ///
    /// O mínimo absoluto fica: um hotspot fraco gera mudanças em rajada, e sem ele isto
    /// transformava o recuo em nada e o vigia num martelo.
    fn a_rede_mudou(&mut self) -> usize {
        let quantos = self.espera.len();
        self.espera.clear();
        self.proxima.clear();
        quantos
    }

    /// Discou-se. Agenda a próxima tentativa e faz o recuo crescer — **aconteça o que
    /// acontecer à ligação**. Devolve os segundos que acabou de agendar, para quem os
    /// quiser dizer.
    fn discou(&mut self, peer: &str, agora: std::time::Instant) -> u64 {
        let s = self.espera.entry(peer.to_string()).or_insert(2);
        let agendado = *s;
        self.proxima.insert(
            peer.to_string(),
            agora + std::time::Duration::from_secs(agendado),
        );
        *s = (*s * 2).min(60);
        agendado
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
    // ESPERAR QUE O LADO DE CÁ ESTEJA PRONTO (#172).
    //
    // Esta tarefa arrancava ao mesmo tempo que o `bind()` acabava. Nessa altura ainda não há
    // relay escolhido nem endereço publicado — a documentação do iroh diz isso com todas as
    // letras e oferece o `online()` precisamente para se esperar por ele. As primeiras
    // tentativas falhavam porque NÓS não estávamos prontos, e cada falha duplicava o recuo
    // daquele par: 2, 4, 8, 16 segundos de castigo por uma culpa que não era dele.
    //
    // O tecto de tempo existe para a rede local isolada, onde não há relay nenhum para
    // esperar e a ligação directa funciona à mesma. Quinze segundos é muito mais do que o
    // `online()` costuma demorar e muito menos do que alguém espera por uma conversa.
    let esperou =
        tokio::time::timeout(std::time::Duration::from_secs(15), rede.endpoint.online()).await;
    match esperou {
        Ok(()) => eprintln!("[rede] relay de casa ligado; o vigia começa agora"),
        Err(_) => eprintln!("[rede] 15 s sem relay de casa; o vigia começa na mesma (rede local?)"),
    }

    let mut adiamento = Adiamento::default();
    let mut voltas: u64 = 0;
    // O SINAL DE QUE A MÁQUINA MUDOU DE REDE (#55) — e a primeira versão disto estava
    // ERRADA de uma forma que vale a pena escrever.
    //
    // Eu li o `Endpoint::network_change()` como um `await` que resolve QUANDO a rede muda, e
    // pu-lo num ciclo. Não é isso: é um método que se CHAMA para dizer ao iroh que a rede
    // pode ter mudado — uma entrada, não uma saída. O ciclo estava a mandar o iroh refazer a
    // detecção de rede 44 vezes em 80 segundos, e a escrever «a rede mudou» outras tantas.
    // Não era ruído no registo: era trabalho a mais imposto à biblioteca, para nada.
    //
    // O sinal a sério é o `home_relay_status()`, que é um `Watcher`: o relay de casa mudar
    // de URL, cair ou voltar É a rede a mudar por baixo de nós, e é observável em vez de
    // presumido.
    let (aviso_de_rede, mut mudou_a_rede) = tokio::sync::mpsc::channel::<()>(4);
    {
        let mut relogio = rede.endpoint.home_relay_status();
        tokio::spawn(async move {
            let mut antes = resumo_dos_relays(&relogio.get());
            loop {
                let Ok(agora) = relogio.updated().await else {
                    return;
                };
                let agora = resumo_dos_relays(&agora);
                if agora == antes {
                    continue;
                }
                antes = agora;
                if aviso_de_rede.send(()).await.is_err() {
                    return;
                }
            }
        });
    }
    loop {
        voltas += 1;
        // A REDE MUDOU: o recuo acumulado deixa de fazer sentido, e a volta corre JÁ.
        //
        // E fica escrito no registo com a hora, que é metade do diagnóstico quando alguém
        // do outro hemisfério diz «desligou-se sozinho».
        if mudou_a_rede.try_recv().is_ok() {
            let quantos = adiamento.a_rede_mudou();
            anotar_na_rede(
                &rede,
                &janela,
                format!("a rede desta máquina mudou; {quantos} recuo(s) limpo(s)"),
            );
        }
        let conhecidos: Vec<String> = {
            let Ok(s) = app.servidores.lock() else {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                continue;
            };
            // «ELE FALOU-ME» NÃO É «EU QUERO ENCONTRÁ-LO» (#87).
            //
            // Isto juntava os `peers` de TODOS os servidores — e uma conversa é guardada como
            // um servidor. Quando um estranho me escrevia, o `aplicar` criava a conversa com
            // ele nos `peers`, e a partir daí o vigia discava-lhe de dois em dois segundos,
            // para sempre, mesmo que eu nunca tivesse respondido. Um pedido indesejado
            // transformava-se em tráfego permanente para quem o mandou — e a dizer-lhe, com o
            // próprio tráfego, que eu continuo online.
            //
            // As SALAS continuam todas: lá, estar nos `peers` é prova de ter a chave. Numa
            // CONVERSA a barra é outra — só se disca se EU já lá tiver escrito, que é o único
            // gesto que distingue «aceitei falar contigo» de «recebi uma mensagem tua».
            let minha = app.minha_chave();
            let mut v: Vec<String> = s
                .values()
                .filter(|x| x.com.is_none() || x.autores_provados().contains(&minha))
                .flat_map(|x| x.peers.clone())
                .collect();
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
            let ja_ligado = tem_ligacao_viva(&rede.ligacoes, &peer);
            if ja_ligado {
                // Pegou: o próximo corte volta a tentar depressa, e não daqui a um minuto.
                adiamento.pegou(&peer);
                // A SUBSTITUIÇÃO FORÇADA (#50), só em `debug` e só com a bandeira posta.
                // Sem isto o ramo do `Destino::Substitui` — o único onde o contador de
                // ligados se estragava — não é exercitado por nada nesta máquina.
                if let Some(cada) = crate::bandeiras::discar_a_dobrar_a_cada_voltas() {
                    if voltas.is_multiple_of(cada.max(1)) {
                        // O `ligar` normal recusa-se: o `reservar` ve que o par ja esta no
                        // mapa e volta logo. E preciso passar-lhe por cima, que e
                        // exactamente o que a rede faz sozinha quando os dois lados discam
                        // no mesmo instante.
                        eprintln!(
                            "[teste] a discar A DOBRAR para {} (BRUMA_DISCAR_A_DOBRAR)",
                            &peer[..8.min(peer.len())]
                        );
                        if let Ok(id) = peer.parse::<EndpointId>() {
                            match rede.endpoint.connect(EndpointAddr::from(id), ALPN).await {
                                Ok(conn) => {
                                    let (r, a, j) = (rede.clone(), app.clone(), janela.clone());
                                    tokio::spawn(async move {
                                        let _ = sessao(conn, true, r, a, j).await;
                                    });
                                }
                                Err(e) => eprintln!("[teste] a dobrar falhou: {e}"),
                            }
                        }
                    }
                }
                continue;
            }
            if adiamento.ainda_cedo(&peer, agora) {
                continue;
            }
            // ATENDER NÃO É FUNCIONAR, E O RECUO TEM DE SABER A DIFERENÇA.
            //
            // Isto limpava o recuo no ramo `Ok`. Mas o `ligar` devolve `Ok` assim que faz
            // `tokio::spawn` da sessão — antes de um único byte ir ao fio. «Ok» quer dizer «o
            // aperto de mão do QUIC pegou e a tarefa arrancou», e não «isto serve para alguma
            // coisa».
            //
            // A diferença deixa de ser académica quando a sessão morre logo a seguir: o `Drop`
            // do `SessaoViva` tira-a do mapa, dois segundos depois o vigia vê que não está
            // ligado, disca outra vez, volta a receber `Ok`, e volta a limpar o recuo. Ciclo
            // de dois em dois segundos, para sempre, a dizer ao outro lado com o próprio
            // tráfego que estamos online. Foi assim que um `Sync` grande de mais deixou de ser
            // um erro e passou a ser um martelo.
            //
            // Quem limpa o recuo é o `pegou`, lá em cima, que tem prova de sessão viva.
            let resultado = ligar(&rede, &app, &janela, &peer).await;
            // SE A CULPA É NOSSA, O RECUO NÃO CRESCE (#172).
            //
            // Sem relay de casa não há travessia de NAT nem endereço publicado: a tentativa
            // falha por o lado de cá estar em baixo, não por o outro não atender. Castigar o
            // par por isso é o que fazia um par perfeitamente contactável ficar com um recuo
            // de dezasseis segundos a seguir a uma mudança de rede — precisamente o momento
            // em que se quer religar depressa.
            //
            // Um par que nunca respondeu continua a subir. Um que não foi sequer tentado a
            // sério, não.
            let temos_relay = rede
                .endpoint
                .home_relay_status()
                .get()
                .into_iter()
                .any(|r| r.is_connected());
            let s = if temos_relay {
                adiamento.discou(&peer, agora)
            } else {
                eprintln!(
                    "[rede] {} não atendeu, mas nós é que não temos relay: o recuo fica",
                    &peer[..8.min(peer.len())]
                );
                adiamento.sem_relay(&peer, agora)
            };
            match &resultado {
                Ok(()) => eprintln!(
                    "[rede] discado para {}; se pegar, o recuo limpa-se na volta seguinte",
                    &peer[..8.min(peer.len())]
                ),
                Err(e) => anotar_na_rede(
                    &rede,
                    &janela,
                    format!(
                        "{} não atendeu ({e}); nova tentativa daqui a {s}s",
                        &peer[..8.min(peer.len())]
                    ),
                ),
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

/// Reserva o par para uma tentativa de ligação. `false` quer dizer «já há, ou já vai a
/// caminho» — e nesse caso não se liga outra vez.
/// Este par tem uma ligação VIVA — e não apenas uma entrada no mapa (#142).
///
/// # A avaria que isto fecha
///
/// Três decisões dependiam de a chave estar no mapa: o vigia decidia não discar, o
/// `reservar` recusava uma tentativa, e o desempate preferia a que já lá estava. Mas o que
/// tira uma entrada do mapa é só o `Drop` do `SessaoViva` — e há maneiras de a tarefa da
/// sessão nunca lá chegar. Bastava uma para o par ficar inalcançável **para sempre**: o
/// mapa dizia «ligado», o vigia acreditava, e ninguém voltava a tentar.
///
/// Perguntar ao QUIC é barato e não mente: uma ligação fechada tem razão de fecho. E a
/// entrada morta é removida no mesmo gesto, porque deixá-la lá era o problema.
///
/// Se a leitura for optimista num instante de reconexão interna do iroh, apaga-se uma
/// ligação boa — e o pior caso é o vigia discar outra vez dois segundos depois.
fn tem_ligacao_viva(
    ligacoes: &std::sync::Mutex<std::collections::HashMap<String, (Connection, u64)>>,
    peer: &str,
) -> bool {
    let Ok(mut l) = ligacoes.lock() else {
        return false;
    };
    match l.get(peer) {
        None => false,
        Some((c, _)) if c.close_reason().is_none() => true,
        Some(_) => {
            l.remove(peer);
            eprintln!(
                "[rede] a entrada de {} no mapa estava morta; removida",
                &peer[..8.min(peer.len())]
            );
            false
        }
    }
}

fn reservar(
    ligacoes: &std::sync::Mutex<std::collections::HashMap<String, (Connection, u64)>>,
    a_ligar: &std::sync::Mutex<std::collections::HashSet<String>>,
    peer: &str,
) -> bool {
    let ja = tem_ligacao_viva(ligacoes, peer);
    let Ok(mut fila) = a_ligar.lock() else {
        return false;
    };
    !ja && fila.insert(peer.to_string())
}

/// O anel dos últimos acontecimentos de rede (#119).
///
/// # Porque é que isto existe
///
/// Tudo o que a camada de rede sabe saía num `eprintln!` para o `bruma.log`: «religado a»,
/// «não atendeu», «atrasou-se e perdeu N pedaços», «a rede mudou». Nenhum desses factos
/// chegava à interface — e quem os precisa é a pessoa do outro hemisfério que está a tentar
/// descrever o que viu, não quem tem o ficheiro à mão.
///
/// Duzentos chegam: a esta cadência são horas de conversa, e o ficheiro continua a ter tudo
/// para quem quiser mais.
pub const TECTO_DO_DIARIO: usize = 200;

/// Escreve no registo E no diário que a interface pode mostrar.
///
/// Um sítio só, para não haver acontecimentos que vão a um e não ao outro — que é como
/// metade deles ficou invisível até agora.
pub fn anotar_na_rede(rede: &Rede, janela: &AppHandle, texto: String) {
    eprintln!("[rede] {texto}");
    if let Ok(mut d) = rede.diario.lock() {
        if d.len() >= TECTO_DO_DIARIO {
            d.remove(0);
        }
        d.push((agora_hms(), texto.clone()));
    }
    let _ = janela.emit("rede-aconteceu", texto);
}

/// A hora local em `hh:mm:ss`, para o diário.
#[cfg(windows)]
fn agora_hms() -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!("{:02}:{:02}:{:02}", t.wHour, t.wMinute, t.wSecond)
}
#[cfg(not(windows))]
fn agora_hms() -> String {
    String::new()
}

/// Quanto tempo tem de passar entre dois pedidos de log ao mesmo par (#53).
///
/// Um par cronicamente atrasado — um portátil a nadar, uma ligação por relay saturada —
/// pediria um log inteiro a cada soluço do canal, e cada pedido desses agrava o atraso que
/// o causou. Dez segundos são muito menos do que uma pessoa demora a dar pela falta de uma
/// mensagem, e muito mais do que a rajada de `Lagged` que um único engasgo produz.
const RESYNC_MINIMO: std::time::Duration = std::time::Duration::from_secs(10);

/// Quanto tempo um endereço guardado ainda vale a pena tentar (#118).
///
/// Uma semana. Endereços velhos não são gratuitos: o iroh tenta-os antes de cair no relay, e
/// um caminho morto custa o tempo de esperar por ele. Mas um par que não se vê há uma semana
/// também não perde nada em ser procurado pela descoberta, que é o que já acontecia sempre.
const IDADE_DOS_ENDERECOS: u64 = 7 * 24 * 3600;
/// E quantos, no máximo. Tentar quinze caminhos mortos é pior do que não tentar nenhum.
const MAX_ENDERECOS: usize = 6;

/// Como um endereço de transporte se escreve no índice, e como se volta a ler.
///
/// Texto, e não o tipo do iroh: o índice tem de sobreviver a actualizações da biblioteca, e
/// um `enum` `#[non_exhaustive]` serializado é um convite a que uma versão futura não leia o
/// ficheiro de uma antiga.
fn endereco_para_texto(a: &iroh::TransportAddr) -> Option<String> {
    match a {
        iroh::TransportAddr::Ip(sa) => Some(format!("ip:{sa}")),
        iroh::TransportAddr::Relay(u) => Some(format!("relay:{u}")),
        // Um transporte que esta versão não conhece não se guarda. Guardá-lo como texto
        // opaco seria escrever no disco uma coisa que não se sabe voltar a ler.
        _ => None,
    }
}

fn texto_para_endereco(t: &str) -> Option<iroh::TransportAddr> {
    if let Some(r) = t.strip_prefix("ip:") {
        return r.parse().ok().map(iroh::TransportAddr::Ip);
    }
    if let Some(r) = t.strip_prefix("relay:") {
        return r.parse().ok().map(iroh::TransportAddr::Relay);
    }
    None
}

/// Guarda onde este par foi encontrado, para o voltar a encontrar sem o DNS do n0 (#118).
async fn guardar_onde_ele_estava(rede: &Rede, app: &Arc<App>, peer: &str) {
    let Ok(id) = peer.parse::<EndpointId>() else {
        return;
    };
    let Some(info) = rede.endpoint.remote_info(id).await else {
        return;
    };
    // O QUE SE GUARDA, E POR QUE ORDEM.
    //
    // O `addrs()` vem de um mapa de dispersão: sem ordenar, os seis que se guardam são seis à
    // sorte — e o do relay, que é o único que funciona sempre, podia ficar de fora. Ordena-se
    // com ele à cabeça.
    //
    // **Não** se filtra por `usage()`, e a razão merece ficar escrita: o `TransportAddrUsage`
    // só tem `Active` e `Inactive`, e `Inactive` quer dizer «não está a ser usado AGORA» —
    // que é precisamente o estado do relay quando a ligação directa está a funcionar. Filtrar
    // por ele deitaria fora o endereço mais valioso exactamente na situação em que ele é
    // gratuito de guardar.
    let mut escolhidos: Vec<(u8, String)> = info
        .addrs()
        .filter_map(|a| {
            let peso = if a.addr().is_relay() { 0u8 } else { 1u8 };
            endereco_para_texto(a.addr()).map(|t| (peso, t))
        })
        .collect();
    escolhidos.sort();
    escolhidos.dedup();
    let onde: Vec<String> = escolhidos
        .into_iter()
        .map(|(_, t)| t)
        .take(MAX_ENDERECOS)
        .collect();
    if onde.is_empty() {
        return;
    }
    let visto = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let quantos = onde.len();
    {
        let Ok(mut i) = app.enderecos.lock() else {
            return;
        };
        i.insert(
            peer.to_string(),
            crate::estado::EnderecosDoPar { onde, visto },
        );
    }
    if let Err(e) = app.gravar_indice() {
        eprintln!("[rede] não consegui guardar os endereços de {peer}: {e}");
    } else {
        eprintln!(
            "[rede] guardei {} endereço(s) de {}: sem o DNS do n0, é por aqui que se volta a encontrá-lo",
            quantos,
            &peer[..8.min(peer.len())]
        );
    }
}

/// Onde é que este par estava da última vez — se ainda vale a pena tentar lá (#118).
fn onde_ele_estava(app: &Arc<App>, peer: &str) -> Vec<iroh::TransportAddr> {
    let agora = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let Ok(i) = app.enderecos.lock() else {
        return Vec::new();
    };
    let Some(e) = i.get(peer) else {
        return Vec::new();
    };
    if agora.saturating_sub(e.visto) > IDADE_DOS_ENDERECOS {
        return Vec::new();
    }
    e.onde
        .iter()
        .filter_map(|t| texto_para_endereco(t))
        .take(MAX_ENDERECOS)
        .collect()
}

/// O que define «a mesma rede», para efeitos do #55: que relays estão ligados.
///
/// Compara-se um RESUMO e não o valor inteiro: o `RelayStatus` traz o último erro, que muda
/// sozinho sem a rede ter mudado, e comparar isso daria uma «mudança de rede» a cada
/// tentativa falhada.
fn resumo_dos_relays(rs: &[iroh::endpoint::RelayStatus]) -> Vec<(String, bool)> {
    let mut v: Vec<(String, bool)> = rs
        .iter()
        .map(|r| (r.url().to_string(), r.is_connected()))
        .collect();
    v.sort();
    v
}

/// Acima de quantos ms de escrita se deixa de enfileirar vídeo para um par (#114).
///
/// Meio segundo é muito mais do que uma escrita saudável — no par local ela fica em zero ou
/// um — e muito menos do que o tempo em que um frame ainda interessa. Acima disto, tudo o
/// que se enfileirar chega tarde E segura o texto que vem atrás.
const ESCRITA_LENTA_MS: u64 = 500;

/// Cortar este fragmento de vídeo, ou deixá-lo passar? (#114)
///
/// # A armadilha que esta função existe para não repetir
///
/// A primeira versão perguntava só `escrita_ultima_ms > ESCRITA_LENTA_MS`, e isso era um
/// ferrolho que se fechava sozinho e nunca mais abria: cortar um fragmento significa NÃO
/// escrever, não escrever significa não medir, e não medir significa que a
/// `escrita_ultima_ms` fica presa no valor alto que causou o corte. Uma escrita de 501 ms —
/// trivial num relay saturado — matava a partilha de ecrã para aquele par até ao fim do
/// processo, porque o `contagem` também não é limpo na religação.
///
/// Numa partilha com o microfone silenciado, o vídeo é o único tráfego contínuo: não há mais
/// nada a passar por ali que volte a medir.
///
/// A medida caduca ao fim de `VALIDADE_DA_MEDIDA`. No pior caso perde-se imagem durante dois
/// segundos e mede-se outra vez; em vez de a perder para sempre.
///
/// Está aqui fora, e não no meio do `select!`, porque uma decisão com esta armadilha tem de
/// poder ser testada sozinha — e no laço ela não podia, porque no teste de par há sempre
/// outro tráfego a voltar a medir e a destapar o corte por acidente.
fn corta_video(c: &Contagem, agora: std::time::Instant) -> bool {
    let Some(medida) = c.escrita_medida_em else {
        // Nunca se escreveu nada para este par: não há motivo para cortar.
        return false;
    };
    c.escrita_ultima_ms > ESCRITA_LENTA_MS && agora.duration_since(medida) < VALIDADE_DA_MEDIDA
}

/// Durante quanto tempo uma medida de escrita ainda vale para decidir cortar vídeo.
///
/// Dois segundos. É mais do que o intervalo entre fragmentos de uma partilha a correr, e
/// muito menos do que o tempo em que ficar sem imagem se nota. Ver `escrita_medida_em`.
const VALIDADE_DA_MEDIDA: std::time::Duration = std::time::Duration::from_secs(2);

/// As salas que eu partilho com este par.
///
/// É a mesma pergunta que o sync dirigido faz — «este peer é conhecido nesta sala?» — e a
/// resposta é a lista de logs que faz sentido voltar a oferecer-lhe.
pub fn salas_com(app: &Arc<App>, peer: &str) -> Vec<String> {
    let Ok(s) = app.servidores.lock() else {
        return Vec::new();
    };
    s.values()
        .filter(|srv| srv.peers.iter().any(|p| p == peer))
        .map(|srv| srv.id.clone())
        .collect()
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
    // AS PISTAS DE ONDE ELE ESTAVA (#118).
    //
    // `EndpointAddr::from(id)` é só o identificador: quem descobre o endereço é o serviço de
    // descoberta do preset do n0, por HTTPS e DNS contra servidores deles. Numa app que
    // promete não depender de servidor nenhum, isso é a dependência escondida — sem eles,
    // duas pessoas que já falaram mil vezes não se encontram.
    //
    // Os endereços guardados entram como PISTAS: o iroh tenta-os e cai na descoberta se
    // falharem. Não substituem nada; tiram-lhe a exclusividade.
    let pistas = onde_ele_estava(app, peer);
    let alvo = if pistas.is_empty() {
        EndpointAddr::from(id)
    } else {
        EndpointAddr::from_parts(id, pistas)
    };
    let conn = match rede.endpoint.connect(alvo, ALPN).await {
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

/// Diz à interface QUEM está ligado — a lista inteira, e não «mais um» ou «menos um» (#50).
///
/// # A avaria que isto fecha
///
/// A barra contava eventos: `peer-ligado` somava, `peer-desligado` subtraía. Só que quando os
/// dois lados discam ao mesmo tempo — o que acontece em quase todas as religações, porque os
/// dois vigias correm de dois em dois segundos — o desempate faz `Destino::Substitui`: a
/// entrada do mapa passa a ser da sessão NOVA, e o `Drop` da antiga vê que a série já não é a
/// dela e **cala-se**. A sessão nova emite `peer-ligado`. Uma soma sem a subtração
/// correspondente, uma vez por religação: «3 ligados» numa sala de duas pessoas.
///
/// Um saldo de eventos só precisa de perder um para ficar errado para sempre. A lista não
/// tem esse problema: cada anúncio é a verdade inteira, e o anúncio seguinte corrige o
/// anterior sem ninguém ter de perceber o que se passou entre os dois.
fn anunciar_ligados(rede: &Rede, janela: &AppHandle) {
    // O lock fecha-se ANTES do emit. O `Drop` de uma sessão pode correr com o mapa tomado
    // por quem a está a substituir, e emitir lá dentro seria segurar o mapa durante um
    // salto para a webview.
    let lista: Vec<String> = match rede.ligacoes.lock() {
        Ok(l) => l.keys().cloned().collect(),
        Err(_) => return,
    };
    let _ = janela.emit("peers-ligados", &lista);
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
        // A lista sai SEMPRE que o mapa muda, e só quando ele mudou mesmo. Numa substituição
        // `era_a_nossa` é falso — o mapa já é da sessão nova, que já anunciou — e não há nada
        // a dizer: é precisamente esse o caso em que o saldo de eventos se estragava.
        if era_a_nossa {
            anunciar_ligados(&self.rede, &self.janela);
        }
    }
}

/// Entrega um quadro de vídeo à interface, venha ele por onde vier (#134).
///
/// Existe porque passou a haver dois caminhos: o stream de controlo, que é por onde tudo foi
/// sempre, e os streams unidireccionais, que são para onde isto vai. Ter o porteiro e a
/// contagem escritos duas vezes seria garantir que um dia divergiam — e a metade esquecida
/// seria a do porteiro.
fn entregar_video(
    rede: &Arc<Rede>,
    app: &Arc<App>,
    peer: &str,
    tipo: &str,
    servidor: &str,
    canal: &str,
    dados: Vec<u8>,
) {
    // Imagem de um estranho não abre descodificadores meus. Cada fluxo novo custa um
    // MediaSource, um <video> e um descodificador de hardware; a interface criava-os para
    // qualquer chave que mandasse bytes.
    if !conhecido(app, peer) {
        return;
    }
    if let Ok(mut n) = rede.contagem.lock() {
        n.entry(peer.to_string()).or_default().ecra_rec += 1;
    }
    if tipo == "camara" {
        crate::comandos::camara_recebida(peer, dados);
    } else {
        crate::comandos::ecra_recebido(peer, servidor, canal, dados);
    }
}

/// Quantos streams unidireccionais se aceitam de um par ao mesmo tempo (#134).
///
/// # Porque é que o desenho mudou
///
/// A primeira versão era um stream POR FRAGMENTO, com `read_to_end(8 MiB)` cada um e uma
/// tarefa por stream sem tecto nenhum. Duas coisas más, e a segunda é pior:
///
/// **A memória.** Cem streams abertos ao mesmo tempo são oitocentos megabytes reservados por
/// um par que só precisa de os abrir. O stream de controlo tinha um tecto de 8 MiB no total;
/// aquilo multiplicava-o por quantos o outro lado quisesse.
///
/// **A ordem.** Cada fragmento de ecrã é um segmento fMP4 que o outro lado passa ao
/// `appendBuffer` de um `SourceBuffer`, e essa fila **tem** de estar em ordem. Streams
/// unidireccionais entregam-se independentemente uns dos outros: dois fragmentos em dois
/// streams chegam pela ordem que a rede quiser. Numa rede local isso quase nunca acontece — e
/// foi por isso que a medição do par passou — mas entre os EUA e o Brasil, por relay, é o
/// caso normal. Eu tinha escrito «provado» sobre um teste que não exercitava o problema.
///
/// Agora é UM stream para o vídeo todo, lido em quadros como o de controlo: separa o vídeo do
/// controlo, que era o objectivo — uma mensagem de texto deixa de esperar por um fragmento de
/// ecrã — e mantém a ordem entre fragmentos, que o vídeo exige. Dois de tecto, para o caso de
/// o outro lado abrir mais do que devia.
const MAX_UNI_ABERTOS: usize = 2;

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
    // Entrou no mapa (de novo ou por substituição): a lista mudou.
    anunciar_ligados(&rede, &janela);

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

    // Antes de decidir o que sai e o que entra: ver se este par já escreveu numa sala
    // minha. Se escreveu, é de casa, mesmo que nunca tenhamos sincronizado um com o outro.
    //
    // Corre ANTES do stream, e essa ordem foi aprendida a medir: estava depois, e o
    // `accept_bi` de quem aceita fica à espera para SEMPRE de um par que nunca abra stream
    // nenhum. Tudo o que estivesse a seguir era inalcançável — incluindo os dois porteiros
    // logo abaixo, que eu tinha escrito precisamente para esse caso. Uma sonda que se liga e
    // se cala apanhou oito ligações de pé ao fim de 45 s, sem uma linha no registo.
    aprender_dos_logs(&app, &peer);

    // O TECTO DE ESTRANHOS (#195).
    //
    // Depois do `aprender_dos_logs`, que é o que pode tornar este par de casa, e depois de a
    // sessão já estar no mapa — portanto o próprio par conta a si mesmo, e é por isso que a
    // comparação é `>` e não `>=`.
    //
    // Quem é de casa nunca é recusado. Quem não é, e chega quando já há cinco desconhecidos
    // pendurados, leva com a porta na cara — e a porta diz porquê no registo, senão isto
    // seria mais um desaparecimento sem explicação.
    //
    // E só nas ligações que ENTRAM. Uma que eu inicio foi decisão minha — o vigia só disca a
    // quem está nos `peers` de alguma sala minha, ou a um amigo — e recusá-la seria o tecto a
    // cortar-me a mim próprio por eu ter tido azar na ordem das ligações.
    if !iniciei {
        let ligados: Vec<String> = rede
            .ligacoes
            .lock()
            .map(|l| l.keys().cloned().collect())
            .unwrap_or_default();
        if ha_estranhos_a_mais(&app, &ligados, &peer) {
            eprintln!(
                "[porteiro] {} liga-se sem partilhar sala nenhuma e já há {TECTO_DE_ESTRANHOS} \
                 assim: recuso",
                &peer[..8.min(peer.len())]
            );
            conn.close(0u32.into(), b"demasiados desconhecidos");
            return Ok(());
        }
    }

    // O ANÚNCIO DE QUANTOS PEDAÇOS DE VOZ MANDEI (#124).
    //
    // De dois em dois segundos, e só enquanto houver voz a sair para este par. É o que dá ao
    // outro lado a metade que lhe falta para calcular a perda: ele conta os que chegaram, eu
    // digo quantos saíram, e a diferença é o que se perdeu no caminho. Sem isto, o receptor
    // não tem como distinguir «ele calou-se» de «perdi trinta pacotes».
    //
    // Dois segundos é o intervalo em que um número destes ainda é útil e não enche nada: são
    // uns 100 bytes a cada dois segundos, contra os ~24 kbps da própria voz.
    {
        let rede_vozes = rede.clone();
        let quem = peer.clone();
        guarda.tarefas.push(tokio::spawn(async move {
            let mut ultimo = 0u64;
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                let onde = rede_vozes.presenca.lock().ok().and_then(|p| p.clone());
                let Some((servidor, Some(canal))) = onde else {
                    continue;
                };
                let enviados = rede_vozes
                    .contagem
                    .lock()
                    .ok()
                    .and_then(|c| c.get(&quem).map(|n| n.voz_env))
                    .unwrap_or(0);
                // Nada mudou desde o último anúncio: não se diz outra vez. Numa sala em
                // silêncio isto cala-se sozinho, em vez de repetir o mesmo número.
                if enviados == ultimo {
                    continue;
                }
                ultimo = enviados;
                let _ = rede_vozes.tx.send(Saida::Vozes {
                    para: quem.clone(),
                    servidor,
                    canal,
                    enviados,
                });
            }
        }));
    }

    // O PRAZO SÓ SE APLICA A QUEM AINDA NÃO É DE CASA — e essa isenção não é cortesia.
    //
    // A primeira versão disto armava o prazo para toda a gente, e matava sessões legítimas.
    // A bandeira `ola_chegou` só é levantada pelo LEITOR, e o leitor só nasce depois de o
    // sync inicial inteiro ter saído: um `await` por cada lote de 256 KiB, por cada sala
    // partilhada. Este mesmo ficheiro dá 20 s a CADA um desses quadros, e a razão escrita ao
    // lado do `prazo_de_escrita` é «um sync legítimo entre o Brasil e os EUA pode ser grande
    // e lento». Dois lotes lentos e o prazo de 30 s estoura — com o `Ola` do outro lado a ter
    // chegado no primeiro segundo, à espera de ser lido.
    //
    // O resultado seria um par derrubado a meio do sync, a religar-se, a recomeçar do
    // princípio e a ser cortado outra vez. Para sempre. E não apareceu na medição porque a
    // cópia dos dados reais tem 115 entradas, onde esse passo é instantâneo.
    //
    // A correcção é a isenção, e ela CHEGA — não por sorte, mas por uma propriedade que já
    // existia: para um desconhecido não há sync nenhum a escrever. O `pacotes` só junta salas
    // onde o par já é conhecido, e a `Presenca` só sai a quem `participa`. Ou seja, o caminho
    // entre armar o prazo e nascer o leitor é, para um desconhecido, o `Ola` e mais nada.
    // Trinta segundos continuam a ser de sobra para isso.
    //
    // Quem tem um sync grande à frente é sempre gente de casa — e essa não é vigiada. É a
    // mesma isenção que o tecto aqui em cima já tinha, e cuja ausência aqui era a assimetria
    // que devia ter-me saltado à vista.
    //
    // (Considerei mover o leitor para antes do sync, que tornaria a bandeira independente
    // disto tudo. Não o fiz: é uma reordenação de uma função que já foi reordenada uma vez
    // nesta mesma fase, e o proveito seria zero enquanto a isenção estiver de pé. Fica dito
    // para o dia em que alguém queira vigiar também os de casa — nesse dia, o leitor tem de
    // subir primeiro.)
    let vigiar_o_prazo = !e_de_casa(&app, &peer);

    // O PRAZO PARA PROVAR QUE É GENTE (#195) — armado ANTES da espera pelo stream.
    //
    // Estava depois, e não servia de nada: quem nunca abre um stream nunca chega lá. Aqui
    // cobre as duas esperas que um par pode fazer eternas — o stream e o `Ola` — porque
    // fechar a ligação faz o `accept_bi` falhar e a sessão sair pelo caminho normal, com o
    // `SessaoViva` a limpar tudo.
    //
    // Trinta segundos e não três: entre o Brasil e os EUA, com o aperto de mão do QUIC e um
    // arranque de app pelo meio, um par legítimo pode demorar. O que isto corta é quem NUNCA
    // fala, não quem fala devagar.
    let ola_chegou = Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let ola_chegou = ola_chegou.clone();
        let conn_prazo = conn.clone();
        let quem = peer.clone();
        guarda.tarefas.push(tokio::spawn(async move {
            tokio::time::sleep(PRAZO_DO_OLA).await;
            if vigiar_o_prazo && !ola_chegou.load(std::sync::atomic::Ordering::Relaxed) {
                eprintln!(
                    "[porteiro] {} ligou-se e não se apresentou em {PRAZO_DO_OLA:?}: fecho",
                    &quem[..8.min(quem.len())]
                );
                conn_prazo.close(0u32.into(), b"sem ola");
            }
        }));
    }
    // A QUEDA DE UMA SESSÃO DE PÉ (#56). Nesta máquina a rede não soluça, e sem isto a
    // religação — o caso normal entre os EUA e o Brasil — nunca é exercitada por nada.
    if let Some(ms) = crate::bandeiras::sessao_morre_ao_fim_de() {
        let conn_morte = conn.clone();
        let quem = peer.clone();
        guarda.tarefas.push(tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
            eprintln!(
                "[teste] a derrubar a sessão de {} ao fim de {ms} ms (BRUMA_SESSAO_MORRE_MS)",
                &quem[..8.min(quem.len())]
            );
            conn_morte.close(0u32.into(), b"queda de proposito");
        }));
    }

    // E ONDE ELE ESTÁ AGORA fica guardado (#118): a sessão está de pé, portanto o que o
    // iroh sabe sobre este par acabou de ser confirmado pela realidade.
    {
        let r = rede.clone();
        let a = app.clone();
        let quem = peer.clone();
        guarda.tarefas.push(tokio::spawn(async move {
            guardar_onde_ele_estava(&r, &a, &quem).await;
        }));
    }

    // O SEGUNDO CAMINHO PARA O VÍDEO (#134), passo 1 de dois: LER, sem ainda escrever.
    //
    // # O problema
    //
    // A sessão abre um `open_bi` e tudo passa por lá: o `Ola`, o histórico completo de cada
    // sala, cada mensagem nova, os sinais, e cada fragmento de ecrã e de câmara — até 8 MB.
    // Um stream QUIC é fiável e ORDENADO: enquanto um fragmento de ecrã de centenas de
    // kilobytes não passar, **nada passa atrás dele**. É a mesma causa do `Lagged` que
    // deitava fora mensagens de texto (#53), vista do outro lado do fio.
    //
    // # Porque é que isto é lido antes de ser escrito
    //
    // Se eu mandasse vídeo por aqui já, uma v0.20.0 do outro lado não o leria: ela só sabe
    // do stream único, e a partilha de ecrã morria em silêncio para quem não tivesse
    // actualizado. É o mesmo procedimento de duas versões do `ESCREVER_PRESO_AO_AUTOR`:
    // primeiro sai uma versão que LÊ as duas coisas e escreve a antiga; quando as duas
    // máquinas estiverem nela — e a app mostra a versão do outro lado desde a v0.18.0 —,
    // vira-se a escrita.
    //
    // Enquanto o `#[cfg]` da escrita não virar, este leitor nunca recebe nada. Isso é o
    // esperado, e não uma avaria.
    {
        let conn_uni = conn.clone();
        let rede_uni = rede.clone();
        let app_uni = app.clone();
        let quem = peer.clone();
        guarda.tarefas.push(tokio::spawn(async move {
            let mut abertos = 0usize;
            loop {
                let Ok(mut recebe) = conn_uni.accept_uni().await else {
                    return;
                };
                abertos += 1;
                if abertos > MAX_UNI_ABERTOS {
                    // Mais do que isto é um par a abrir streams à custa da minha memória.
                    // Fecha-se sem ler, que é o que o porteiro faria se pudesse correr antes
                    // dos bytes.
                    eprintln!(
                        "[porteiro] {} abriu mais de {MAX_UNI_ABERTOS} streams de vídeo: fecho",
                        &quem[..8.min(quem.len())]
                    );
                    return;
                }
                let rede_uni = rede_uni.clone();
                let app_uni = app_uni.clone();
                let quem = quem.clone();
                // O stream é lido em QUADROS, com o mesmo enquadramento do stream de
                // controlo — e não de uma vez até ao fim. É isso que mantém a ordem entre
                // fragmentos e o tecto de memória num quadro de cada vez.
                tokio::spawn(async move {
                    loop {
                        match ler(&mut recebe).await {
                            Ok(Quadro::Video {
                                tipo,
                                servidor,
                                canal,
                                dados,
                            }) => entregar_video(
                                &rede_uni, &app_uni, &quem, &tipo, &servidor, &canal, dados,
                            ),
                            // Um quadro que não é vídeo neste stream vem de uma versão que
                            // ainda não existe: ignora-se, como o `Msg::Desconhecida`.
                            Ok(_) => continue,
                            Err(_) => return,
                        }
                    }
                });
            }
        }));
    }

    let ola_para_o_leitor = ola_chegou.clone();

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
                let e = n.entry(voz_peer.clone()).or_default();
                e.voz_rec += 1;
                // O INSTANTE, e não só a soma (#33). É ele que distingue «ele está calado»
                // de «ele acha que está a falar» — dois problemas com respostas opostas.
                e.ultimo_rec = Some(std::time::Instant::now());
            }
            crate::comandos::voz_recebida(&voz_peer, &d);
        }
    });

    guarda.tarefas.push(ouvinte);

    // O NOME NÃO VAI A QUEM AINDA NÃO É DE CASA (#136, #194).
    //
    // Isto mandava o nome escolhido a QUALQUER pessoa que se ligasse. E ligar-se só precisa do
    // `EndpointId`, que viaja em claro dentro de cada convite — e um convite reencaminha-se.
    // Bastava alguém ter recebido um convite meu de outra pessoa para ficar a saber que estou
    // online e como me chamo.
    //
    // E não se perde nada, porque o nome NUNCA veio daqui para quem interessa: ele viaja
    // dentro do log cifrado, na `Carga::Apresentar`, que o `definir_nome` escreve em todas as
    // salas. Quem partilha uma sala comigo aprende-o por lá — verificado pela chave da sala,
    // que é uma prova, em vez de uma afirmação de quem se liga.
    //
    // O `aprender_dos_logs` já correu acima, portanto neste ponto já se sabe quem é de casa.
    let meu_nome = if e_de_casa(&app, &peer) {
        app.nome.lock().map(|n| n.clone()).unwrap_or_default()
    } else {
        String::new()
    };
    escrever(
        &mut envia,
        &Msg::Ola {
            nome: meu_nome,
            x_pub: Some(HEXLOWER.encode(app.ident.x_public().as_bytes())),
            prekey_sig: Some(HEXLOWER.encode(&app.ident.prekey_signature())),
            versao: Some(env!("CARGO_PKG_VERSION").to_string()),
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
        for m in partir_sync(servidor, entradas) {
            escrever(&mut envia, &m).await?;
        }
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
        // A conta do lixo, por sala, desta sessão (#85).
        let mut orcamento: Orcamento = Default::default();
        let mut ola_visto = false;
        // Um quadro ignorado regista-se uma vez por sessão, não mil.
        let mut quadro_ignorado = false;
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
                    entregar_video(
                        &rede_leitura,
                        &leitura_app,
                        &peer_leitura,
                        &tipo,
                        &servidor,
                        &canal,
                        dados,
                    );
                }
                Ok(Quadro::Controlo(Msg::Ola {
                    nome,
                    x_pub,
                    prekey_sig,
                    versao,
                })) => {
                    // O `emit("peer-nome")` que aqui estava foi removido, e não substituído
                    // por outro (#136). Ninguém o ouvia — e ligá-lo à interface era pior do
                    // que apagá-lo: seria um nome AUTO-DECLARADO por quem se liga, sem prova
                    // nenhuma, a aparecer no ecrã ao lado de nomes que vêm do log cifrado e
                    // esses estão provados pela chave da sala. Duas coisas com o mesmo aspecto
                    // e garantias opostas é como se enganam pessoas.
                    //
                    // O nome continua a chegar, pelo sítio certo: a `Carga::Apresentar`.
                    let _ = &nome;
                    // A VERSÃO DO OUTRO LADO (#4).
                    //
                    // Um par sem este campo corre uma versão anterior à que o introduziu — e
                    // isso também é informação, por isso não se cala: diz-se «anterior a
                    // 0.18». A interface mostra-a e deixa de haver funcionalidades que não
                    // funcionam sem se perceber porquê.
                    let dele = versao.unwrap_or_else(|| "anterior a 0.18".to_string());
                    let minha = env!("CARGO_PKG_VERSION");
                    if dele != minha {
                        eprintln!(
                            "[rede] {} tem a versão {dele} e eu tenho a {minha}",
                            &peer_leitura[..8.min(peer_leitura.len())]
                        );
                    }
                    let _ = leitura_janela.emit("peer-versao", (&peer_leitura, &dele, minha));
                    if ola_visto {
                        // Um par que manda dois `Ola` na mesma sessão está avariado ou a
                        // experimentar — vale a pena sabê-lo, e não vale gravar o disco por ele.
                        eprintln!(
                            "[rede] {} mandou outro Olá na mesma sessão; ignorado",
                            &peer_leitura[..8.min(peer_leitura.len())]
                        );
                    } else {
                        ola_visto = true;
                        // E diz-se ao vigia do prazo que já não precisa de fechar nada.
                        ola_para_o_leitor.store(true, std::sync::atomic::Ordering::Relaxed);
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
                // Um QUADRO que esta versão não sabe ler — um tipo novo, ou um corpo que não
                // desserializa. Ignora-se, pela mesma razão da linha acima, e regista-se UMA
                // vez por sessão: se o outro lado estiver a mandar mil quadros de um tipo novo,
                // mil linhas iguais no registo escondem tudo o resto.
                Ok(Quadro::Desconhecido(porque)) => {
                    if !quadro_ignorado {
                        quadro_ignorado = true;
                        eprintln!(
                            "[rede] {} mandou um quadro que esta versão não lê ({porque});                              ignorado (não volto a dizer nesta sessão)",
                            &peer_leitura[..8.min(peer_leitura.len())]
                        );
                    }
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
                        &mut orcamento,
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
                        &mut orcamento,
                    );
                }
                Ok(Quadro::Controlo(Msg::Vozes {
                    servidor,
                    canal,
                    enviados,
                })) => {
                    // SÓ DE QUEM ESTÁ NA SALA (#124). Um anúncio inflacionado de um estranho
                    // faria a perda parecer enorme — e a perda é um número que se olha para
                    // decidir se a chamada está má. Passa pelo mesmo `participa` que a
                    // presença e o vídeo.
                    if participa(&leitura_app, &servidor, &peer_leitura) {
                        let _ = &canal;
                        if let Ok(mut n) = rede_leitura.contagem.lock() {
                            let e = n.entry(peer_leitura.clone()).or_default();
                            // Só sobe. Um anúncio que venha atrás de outro — reordenação no
                            // controlo, ou uma sessão nova a recomeçar a contagem — não pode
                            // fazer a perda saltar para negativo e ser aparada a zero,
                            // escondendo a perda real que havia antes.
                            e.disse_ter_enviado = e.disse_ter_enviado.max(enviados);
                        }
                    }
                }
                Ok(Quadro::Controlo(Msg::Recusa { servidor })) => {
                    // A RECUSA SÓ CONTA SE VIER DE QUEM PODIA RECUSAR (#131).
                    //
                    // Um terceiro que soubesse o id da conversa — e ele sai de duas chaves
                    // públicas, portanto qualquer pessoa o calcula — mandaria isto para me
                    // fazer crer que a outra pessoa me recusou. Exige-se que quem manda seja
                    // o OUTRO LADO da conversa: é o `com` que o diz, e ele foi escrito por
                    // mim quando abri.
                    let e_dele = leitura_app
                        .servidores
                        .lock()
                        .map(|s| {
                            s.get(&servidor)
                                .is_some_and(|srv| srv.com.as_deref() == Some(&peer_leitura))
                        })
                        .unwrap_or(false);
                    if e_dele {
                        eprintln!(
                            "[porteiro] {} recusou a conversa",
                            &peer_leitura[..8.min(peer_leitura.len())]
                        );
                        let _ = leitura_janela.emit("conversa-recusada", &servidor);
                    }
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

    // O travão do #53 vive aqui, ao lado do laço que o usa: é por sessão, e uma sessão
    // nova tem direito a pedir o log outra vez sem esperar pelo relógio da anterior.
    let mut ultimo_resync: Option<std::time::Instant> = None;
    // O stream por onde o vídeo sai quando o #134 estiver virado. Abre-se à primeira e
    // mantém-se: um por fragmento destruiria a ordem que o fMP4 do outro lado exige.
    let mut canal_de_video: Option<SendStream> = None;
    let nasceu = std::time::Instant::now();
    let janela_lenta = std::time::Duration::from_secs(crate::bandeiras::escrita_lenta_ate_s());
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
                        Saida::Vozes {
                            para,
                            servidor,
                            canal,
                            enviados,
                        } if para == peer => Some(Quadro::Controlo(Msg::Vozes {
                            servidor,
                            canal,
                            enviados,
                        })),
                        Saida::Vozes { .. } => None,
                        Saida::Recusa { para, servidor } if para == peer => {
                            Some(Quadro::Controlo(Msg::Recusa { servidor }))
                        }
                        Saida::Recusa { .. } => None,
                        // Sinalizacao e video sao dirigidos: as outras sessoes deixam passar.
                        Saida::Sinal { para, servidor, canal, dados } if para == peer => {
                            Some(Quadro::Controlo(Msg::Sinal { servidor, canal, dados }))
                        }
                        Saida::Sinal { .. } => None,
                        // O VÍDEO PASSA PELO MESMO PORTEIRO QUE A PRESENÇA (#138).
                        //
                        // Duas linhas acima, a `Saida::Presenca` tem um `participa(...)` com
                        // um comentário a explicar porquê. O vídeo — o ecrã e a câmara, que é
                        // muito mais do que a presença dá — não tinha nada: bastava
                        // `para == peer`, e o `para` sai da lista que o JavaScript mantém.
                        //
                        // Quem forjasse presença entrava nessa lista e passava a receber o
                        // meu ecrã. O guarda estava construído e esta porta ficou de fora.
                        Saida::Video { tipo, para, servidor, canal, dados }
                            if para == peer && participa(&app, &servidor, &peer) =>
                        {
                            // A ESCRITA ESTÁ ATRASADA? ENTÃO ESTE FRAGMENTO NÃO VAI (#114).
                            //
                            // Um fragmento que fica na fila enquanto a escrita não passa é
                            // duas coisas más ao mesmo tempo: chega atrasado — e um frame
                            // atrasado não interessa a ninguém — e segura o texto que vem
                            // atrás dele, porque é tudo o mesmo stream ordenado.
                            //
                            // Deitá-lo fora parte a imagem. Parte no presente, que é a
                            // única altura em que partir serve para alguma coisa.
                            let atrasado = rede
                                .contagem
                                .lock()
                                .ok()
                                .and_then(|n| {
                                    n.get(&peer)
                                        .map(|c| corta_video(c, std::time::Instant::now()))
                                })
                                .unwrap_or(false);
                            if atrasado {
                                if let Ok(mut n) = rede.contagem.lock() {
                                    // O corte conta-se por tipo (revisão): o `video_cortado`
                                    // somava ecrã e câmara enquanto o `ecra_env` já era só
                                    // ecrã, e a percentagem do aviso de rede inflacionava
                                    // com a câmara ligada.
                                    let e = n.entry(peer.clone()).or_default();
                                    if tipo == "ecra" {
                                        e.video_cortado += 1;
                                    } else {
                                        e.camara_cortada += 1;
                                    }
                                }
                                None
                            } else {
                            if let Ok(mut n) = rede.contagem.lock() {
                                // O ecrã e a câmara contam-se à parte (#133): passam pelo
                                // mesmo braço, e o `ecra_env` somava os dois.
                                let e = n.entry(peer.clone()).or_default();
                                if tipo == "ecra" {
                                    e.ecra_env += 1;
                                } else {
                                    e.camara_env += 1;
                                }
                            }
                            let q = Quadro::Video {
                                tipo: tipo.to_string(),
                                servidor,
                                canal,
                                dados: dados.as_ref().clone(),
                            };
                            // CADA FRAGMENTO NO SEU STREAM (#134, passo 2).
                            //
                            // Sai daqui sem passar pelo `escrever_quadro`, portanto sem
                            // segurar o stream de controlo: uma mensagem de texto deixa de
                            // esperar por um fragmento de ecrã de centenas de kilobytes.
                            //
                            // E cada um na sua tarefa, porque abrir e escrever um stream
                            // também espera — se isso corresse aqui, o laço do `select!`
                            // ficava parado e voltávamos ao princípio.
                            if crate::bandeiras::video_por_uni() {
                                // UM stream para o vídeo todo, aberto à primeira vez e
                                // mantido. Separa o vídeo do controlo — que é o objectivo —
                                // sem lhe destruir a ordem, que o fMP4 do outro lado exige.
                                //
                                // Escreve-se AQUI e não numa tarefa: pôr isto a correr fora
                                // do laço reordenava os fragmentos entre si, que é
                                // exactamente o que este desenho existe para evitar. E o que
                                // fica à espera passa a ser o vídeo e não o texto, porque o
                                // texto já não vem por aqui.
                                if canal_de_video.is_none() {
                                    canal_de_video = conn.open_uni().await.ok();
                                }
                                if let Some(uni) = canal_de_video.as_mut() {
                                    if escrever_quadro(uni, &q).await.is_err() {
                                        canal_de_video = None;
                                    }
                                }
                                None
                            } else {
                                Some(q)
                            }
                            }
                        }
                        Saida::Video { .. } => None,
                    };
                    if let Some(q) = quadro {
                        // A ESCRITA LENTA DE PROPÓSITO (#53), só em `debug` e só com a
                        // bandeira posta: é o que faz o canal de difusão transbordar e
                        // dizer `Lagged`, que nesta máquina nunca acontece sozinho.
                        //
                        // E é TRANSITÓRIA, com uma janela fixa. Um atraso permanente também
                        // afogaria a recuperação — o `SyncPara` que o `Lagged` enfileira sai
                        // por este mesmo laço — e o teste diria «não recuperou» sobre uma
                        // recuperação que nunca teve por onde sair. Uma rede que engasga e
                        // volta ao normal é o caso real; uma que nunca mais escreve é outro
                        // problema, e tem outro nome.
                        // CRONOMETRAR A ESCRITA (#114). É o número que explica o
                        // `Lagged`: enquanto isto espera, a sessão não lê do canal.
                        //
                        // O atraso de propósito fica DENTRO da medição, e isso foi uma
                        // correcção: estava antes do `let antes`, portanto a bandeira que
                        // existe para simular uma escrita lenta era a única coisa que a
                        // medição não via — e o ramo do corte de vídeo, que depende dela,
                        // não era exercitado por nada.
                        let antes = std::time::Instant::now();
                        if let Some(ms) = crate::bandeiras::atraso_da_escrita_ms() {
                            if nasceu.elapsed() < janela_lenta {
                                tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                            }
                        }
                        let falhou = escrever_quadro_partido(&mut envia, q).await.is_err();
                        let demorou = antes.elapsed().as_millis() as u64;
                        if let Ok(mut n) = rede.contagem.lock() {
                            let e = n.entry(peer.clone()).or_default();
                            e.escrita_ultima_ms = demorou;
                            e.escrita_medida_em = Some(std::time::Instant::now());
                            e.escrita_pior_ms = e.escrita_pior_ms.max(demorou);
                        }
                        if falhou {
                            break;
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    // O NÚMERO CHEGA À INTERFACE (#166), em vez de ficar só no registo: é a
                    // única prova de que a imagem partida daquele momento tem explicação.
                    if let Ok(mut mapa) = rede.contagem.lock() {
                        mapa.entry(peer.clone()).or_default().perdidos_no_canal += n;
                    }
                    // POR AQUI NÃO PASSA SÓ IMAGEM: PASSA TEXTO (#53).
                    //
                    // O comentário que aqui estava dizia que os frames de ecrã e de câmara
                    // não se recuperam — e é verdade, um frame perdido já não interessa a
                    // ninguém. Mas por este mesmo canal passa a `Saida::Entrada`, que é uma
                    // MENSAGEM NOVA. Essa perdia-se exactamente igual, em silêncio, e não
                    // havia nada que a fosse buscar: o `Sync` completo só acontece no
                    // arranque da sessão. Numa app cujo requisito é «as mensagens sobrevivem
                    // a estar offline», perder uma por o canal se ter atrasado é o pior
                    // género de defeito — silencioso e sem rasto.
                    //
                    // Cair também não serve: derrubar a ligação porque um espectador se
                    // atrasou é trocar um soluço na imagem por uma chamada perdida.
                    //
                    // O que se faz é separar o que se recupera do que não se recupera. O
                    // `merge` é idempotente e endereçado por conteúdo, portanto pedir o log
                    // outra vez não custa nada a quem já o tem.
                    anotar_na_rede(
                        &rede,
                        &janela,
                        format!(
                            "{} atrasou-se e perdeu {n} pedaços",
                            &peer[..8.min(peer.len())]
                        ),
                    );
                    // O TRAVÃO. Um par cronicamente atrasado pediria sincronizações de logs
                    // grandes a cada soluço, o que agrava o próprio atraso que as causou.
                    let agora = std::time::Instant::now();
                    let pode = ultimo_resync
                        .map(|t: std::time::Instant| agora.duration_since(t) >= RESYNC_MINIMO)
                        .unwrap_or(true);
                    if pode {
                        ultimo_resync = Some(agora);
                        let salas = salas_com(&app, &peer);
                        if !salas.is_empty() {
                            eprintln!(
                                "[rede] a pedir o log de {} sala(s) a {}: o texto perdido volta por aqui",
                                salas.len(),
                                &peer[..8.min(peer.len())]
                            );
                        }
                        for servidor in salas {
                            let _ = rede.tx.send(Saida::SyncPara {
                                para: peer.clone(),
                                servidor,
                            });
                        }
                    }
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

/// Onde a interface está, para um estado inconsistente poder ser DITO.
///
/// Os porteiros não têm `AppHandle` — são funções puras chamadas de todo o lado. Isto é o
/// único fio para lhes dar voz, e é posto uma vez no arranque da rede.
static JANELA: std::sync::OnceLock<AppHandle> = std::sync::OnceLock::new();

/// «Recusado» e «não consegui perguntar» NÃO SÃO A MESMA COISA (#139).
///
/// # A avaria que isto torna visível
///
/// Os três porteiros acabavam todos em `.unwrap_or(false)`. Se um `unwrap()` em qualquer
/// outro sítio entrar em pânico enquanto segura o `app.servidores` — e há vários — o mutex
/// fica ENVENENADO, e daí em diante todos os porteiros dizem que não. A toda a gente. Para
/// sempre. Sem uma linha em lado nenhum.
///
/// O sintoma para quem está a usar: a voz cala-se, a sincronização pára, ninguém aparece na
/// sala — e tudo isto parece um problema de rede, que é o sítio errado para procurar. Uma app
/// sem servidor não tem a quem perguntar o que se passa.
///
/// Um envenenamento é um ESTADO DA APP, não uma resposta. Continua a devolver-se o valor
/// seguro (recusar), mas diz-se: uma vez no registo, e uma faixa na interface a dizer que
/// fechar e voltar a abrir resolve — porque resolve mesmo, o log está no disco.
///
/// # Porque é que não se termina o processo
///
/// Foi ponderado, e é defensável: um envenenamento é irrecuperável e continuar a fingir é
/// pior. Não se faz porque a queda seria a MEIO de uma chamada, e um par que cai sem dizer
/// nada é exactamente o que este projecto passou versões a corrigir. A faixa aparece, a
/// pessoa escolhe o momento, e o estado no disco não corre risco nenhum entretanto.
fn com_servidores<T>(
    app: &Arc<App>,
    o_que: &str,
    seguro: T,
    f: impl FnOnce(&std::collections::BTreeMap<String, crate::estado::Servidor>) -> T,
) -> T {
    match app.servidores.lock() {
        Ok(s) => f(&s),
        Err(_) => {
            dizer_que_o_estado_esta_partido(o_que);
            seguro
        }
    }
}

/// Diz-se UMA vez — e nas duas direcções, registo e interface.
///
/// Uma vez porque um porteiro envenenado é consultado a cada datagrama de voz: repetir
/// encheria o registo em segundos e tornaria ilegível justamente o ficheiro onde a
/// explicação está.
fn dizer_que_o_estado_esta_partido(o_que: &str) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static JA: AtomicBool = AtomicBool::new(false);
    if JA.swap(true, Ordering::Relaxed) {
        return;
    }
    eprintln!(
        "[estado] o mapa de servidores ficou envenenado (a perguntar por «{o_que}»): a partir \
         de agora os porteiros recusam tudo. Fecha e volta a abrir o Bruma."
    );
    if let Some(j) = JANELA.get() {
        let _ = j.emit(
            "erro-dados",
            "O Bruma ficou num estado inconsistente: a voz, o vídeo e a sincronização vão \
             recusar tudo até fechares e voltares a abrir. Os teus dados no disco estão bem.",
        );
    }
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
    com_servidores(app, "conhecido", false, |s| {
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
}

/// Já há desconhecidos a mais para aceitar mais este? (#195)
///
/// À parte da `sessao` pela razão de sempre neste ficheiro: é uma DECISÃO, e uma decisão
/// testa-se sozinha. A `sessao` precisa de uma ligação QUIC viva; isto precisa de uma lista de
/// nomes.
///
/// Duas regras, e a ordem entre elas é o ponto todo:
///
/// 1. **Quem é de casa entra sempre.** Por muito cheia que a casa esteja, um par que partilha
///    uma sala comigo — ou que eu convidei — nunca é recusado. Um tecto que corta gente de
///    casa é uma avaria, não uma defesa.
/// 2. Para os outros, conta-se quantos DESCONHECIDOS já estão ligados. O `ligados` inclui esta
///    sessão, que já se registou no mapa — por isso o próprio par a contar-se a si mesmo é o
///    que faz a comparação ser `>` e não `>=`.
fn ha_estranhos_a_mais(app: &Arc<App>, ligados: &[String], quem_chega: &str) -> bool {
    if e_de_casa(app, quem_chega) {
        return false;
    }
    ligados.iter().filter(|p| !e_de_casa(app, p)).count() > TECTO_DE_ESTRANHOS
}

/// Fui EU que o convidei para alguma sala? — o outro fio de confiança, além de partilhar sala.
///
/// Quem entra por convite ainda não escreveu nada, portanto não é `conhecido` de ninguém. Do
/// lado de quem entrou, o anfitrião fica em `convidou`; é esse o único par que se conhece antes
/// de haver prova. Serve para decidir a quem se diz o nome (#136) e quem não conta para o tecto
/// de estranhos (#195).
fn e_de_casa(app: &Arc<App>, peer: &str) -> bool {
    conhecido(app, peer)
        || com_servidores(app, "convidou", false, |s| {
            s.values().any(|srv| srv.convidou.as_deref() == Some(peer))
        })
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
    com_servidores(app, "pode sincronizar", false, |s| {
        s.get(servidor).is_some_and(|srv| {
            srv.peers.iter().any(|p| p == peer) || srv.convidou.as_deref() == Some(peer)
        })
    })
}

/// A lista de destinatários, depois do porteiro — e a queixa quando alguém cai fora.
///
/// Usada pela voz (#138). Devolve só quem participa mesmo naquela sala, e diz UMA vez quando
/// tira alguém: se a interface e o Rust discordam sobre quem está numa chamada, isso é uma
/// coisa que se quer saber, e não um silêncio a cada pedaço de som.
pub fn so_quem_participa(app: &Arc<App>, servidor: &str, para: &[String]) -> Vec<String> {
    let permitidos: Vec<String> = para
        .iter()
        .filter(|p| participa(app, servidor, p))
        .cloned()
        .collect();
    if permitidos.len() != para.len() {
        use std::sync::atomic::{AtomicBool, Ordering};
        static JA: AtomicBool = AtomicBool::new(false);
        if !JA.swap(true, Ordering::Relaxed) {
            eprintln!(
                "[porteiro] a interface queria mandar voz a {} pares e só {} pertencem à sala \
                 {}: os outros não recebem",
                para.len(),
                permitidos.len(),
                &servidor[..8.min(servidor.len())]
            );
        }
    }
    permitidos
}

fn participa(app: &Arc<App>, servidor: &str, peer: &str) -> bool {
    com_servidores(app, "participa", false, |s| {
        s.get(servidor)
            .is_some_and(|srv| srv.peers.iter().any(|p| p == peer))
    })
}

/// Quanto lixo cada par já nos fez engolir, nesta sessão e por sala (#85).
///
/// Vive na sessão e não no `App` de propósito: um par que se porte mal e depois se ligue outra
/// vez merece uma segunda oportunidade — o que não pode é gastar-nos a máquina numa só.
type Orcamento = std::collections::HashMap<(String, String), usize>;

fn aplicar(
    app: &Arc<App>,
    janela: &AppHandle,
    servidor: &str,
    entradas: Vec<blog::Entry>,
    peer: &str,
    rede: &Arc<Rede>,
    orcamento: &mut Orcamento,
) {
    // O PORTEIRO DA ENTRADA (#85).
    //
    // Quem já provou pertencer a esta sala passa sem limite nenhum: sincronizar um histórico
    // grande é o comportamento normal e não pode ter tecto. Quem ainda não provou entra na
    // mesma — é assim que alguém se prova, escrevendo algo que decifra — mas com uma conta
    // aberta, e a conta é do lixo que já mandou.
    //
    // O tecto conta o lixo JÁ GASTO **mais** o que este lote pode gastar no pior caso, que é
    // ele inteiro. Sem a segunda metade, o primeiro lote de cada sessão era ilimitado — e uma
    // religação repunha a conta a zero, portanto bastava religar para voltar a ter direito a
    // um lote sem tecto. O que este porteiro existe para impedir é precisamente a app a
    // congelar a decifrar um quadro cheio de lixo, e o primeiro quadro é o que congela.
    //
    // Quem já provou pertencer à sala continua sem limite nenhum: sincronizar um histórico
    // grande é o comportamento normal e não pode ter tecto.
    let chave = (peer.to_string(), servidor.to_string());
    if !pode_sincronizar(app, servidor, peer) {
        let gasto = orcamento.get(&chave).copied().unwrap_or(0);
        if gasto + entradas.len() > LIXO_TOLERADO {
            // Conta-se a tentativa, senão bastava repetir para nunca esgotar.
            *orcamento.entry(chave).or_insert(0) += entradas.len();
            return;
        }
    }
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
            // E DIZ-SE-LHE (#131). Sem isto ele escrevia dias para uma sala que só existe
            // na máquina dele.
            let _ = rede.tx.send(Saida::Recusa {
                para: peer.to_string(),
                servidor: servidor.to_string(),
            });
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

    // Quem passou a ser membro desta sala AGORA (#196). Sai do bloco com o mutex para ser
    // dito depois de ele ser largado: emitir um evento com um `lock` na mão é como se
    // constroem os bloqueios que ninguém consegue reproduzir.
    let mut entraram: Vec<String> = Vec::new();
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
        let novas = match srv.merge_contado(entradas) {
            Ok((n, recusadas)) => {
                if recusadas > 0 {
                    let gasto = orcamento.entry(chave.clone()).or_insert(0);
                    *gasto += recusadas;
                    if *gasto > LIXO_TOLERADO {
                        eprintln!(
                            "[porteiro] {} mandou {} entradas que não decifram na sala {}: \
                             fecho-lhe a conta desta sala nesta sessão",
                            &peer[..8.min(peer.len())],
                            gasto,
                            &servidor[..8.min(servidor.len())]
                        );
                    }
                }
                n
            }
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
            for autor in &novos {
                if autor == peer {
                    aprendi = true;
                }
                srv.peers.push(autor.clone());
            }
            entraram = novos;
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

    // ALGUÉM PASSOU A SER MEMBRO, E ISSO DIZ-SE (#196).
    //
    // Quando uma entrada nova decifra, todos os autores dela passam a estar nos `peers` — e é
    // a regra certa, porque a chave da sala é a prova. Mas o acontecimento era completamente
    // mudo: nem evento, nem linha no histórico, nem nada. Alguém ganhava o direito de me pôr
    // som nas colunas e de receber o meu ecrã, e ninguém ficava a saber.
    //
    // Vai como AVISO e não como mensagem: não é história assinada, é do mesmo tipo efémero
    // que a presença já é. E vai em lote — numa sala que sincroniza histórico grande, muitos
    // autores aparecem de uma vez, e uma linha por cada seria uma enxurrada em vez de uma
    // informação.
    if !entraram.is_empty() {
        let _ = janela.emit("membros-novos", (servidor, &entraram));
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
    /// Um quadro que esta versão não sabe ler — tipo desconhecido, ou corpo que não
    /// desserializa. Ver o `ler()`: NÃO é um erro, é uma coisa a ignorar.
    Desconhecido(&'static str),
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

/// O nome de uma mensagem, para uma queixa poder dizer QUAL delas cresceu.
///
/// Um erro que diz só «um quadro de 9 MiB» manda quem o lê procurar no ficheiro inteiro. A
/// etiqueta é a mesma que vai no `t` do JSON, portanto o que se lê no registo é o que se
/// procura no código.
fn nome_da_msg(m: &Msg) -> &'static str {
    match m {
        Msg::Ola { .. } => "Ola",
        Msg::Sync { .. } => "Sync",
        Msg::Nova { .. } => "Nova",
        Msg::Presenca { .. } => "Presenca",
        Msg::Sinal { .. } => "Sinal",
        Msg::Recusa { .. } => "Recusa",
        Msg::Vozes { .. } => "Vozes",
        Msg::Desconhecida => "Desconhecida",
    }
}

/// Parte um `Sync` em tantos quantos forem precisos para cada um caber num quadro.
///
/// **Não é uma variante nova do protocolo, e isso é o ponto.** Uma versão anterior recebe
/// vários `Msg::Sync` do mesmo servidor em vez de um, e junta cada um como já juntava — o
/// `merge_verificado` é por entrada e a deduplicação sai do hash. Um par na v0.18.1 entende
/// isto sem saber que existe, e sem precisar de actualizar.
///
/// E não multiplica a retribuição: o `Saida::SyncPara` sai de `aprendi`, que só é verdade da
/// primeira vez que um par se prova naquela sala. Os lotes seguintes não a voltam a disparar.
///
/// Um `Sync` VAZIO continua a ir: é ele que diz «este servidor é meu e não tenho nada», e o
/// simulador de ataque depende de o poder mandar.
fn partir_sync(servidor: String, entradas: Vec<blog::Entry>) -> Vec<Msg> {
    if entradas.is_empty() {
        return vec![Msg::Sync {
            servidor,
            entradas: Vec::new(),
        }];
    }
    // O que o envelope custa: `{"t":"Sync","servidor":"…","entradas":[]}` mais o byte do
    // tipo de quadro. Calcula-se uma vez e serve para saber se uma entrada CABE.
    let envelope = 1 + serde_json::to_vec(&Msg::Sync {
        servidor: servidor.clone(),
        entradas: Vec::new(),
    })
    .map(|v| v.len())
    .unwrap_or(64);

    let mut lotes: Vec<Msg> = Vec::new();
    let mut atual: Vec<blog::Entry> = Vec::new();
    let mut tam = 0usize;
    let mut indeliveraveis = 0usize;
    for e in entradas {
        // O tamanho REAL da entrada depois de serializada, e não uma estimativa: o
        // `ciphertext` vai em hexadecimal, e uma mensagem no limite dos 4000 caracteres ocupa
        // dezenas de KiB. Uma média não protege contra o caso que interessa, que é o extremo.
        let n = serde_json::to_vec(&e).map(|v| v.len()).unwrap_or(0) + 1;

        // UMA ENTRADA QUE SOZINHA NÃO CABE NUM QUADRO NÃO SE PARTE — parte-se por entradas.
        //
        // O empacotamento fecha o lote antes de uma entrada grande, portanto ela acaba sozinha
        // no seu; e um lote de uma entrada só tem o tamanho dessa entrada, que pode passar o
        // `MAX_FRAME`. Aí o `corpo_do_quadro` recusa-se a construir o quadro, o `?` do aperto
        // de mão propaga, e a sessão morre — **antes** da `Presenca` e antes do `select!`.
        // Como o histórico não encolhe, morria outra vez a cada religação: seria o #213 a
        // voltar por uma porta mais estreita.
        //
        // Que uma entrada assim exista é possível: o `merge_verificado` guarda tudo o que
        // decifra e assina, sem tecto de tamanho, e um `Msg::Nova` cabe em três bytes a menos
        // do que o `Msg::Sync` equivalente (`"entrada":E` contra `"entradas":[E]`). Ou seja,
        // há uma janela em que uma entrada ENTRA por `Nova` e depois não SAI por `Sync`.
        //
        // Salta-se, e diz-se. Aquela entrada é indeliverável por construção — não há quadro
        // que a leve —, mas o resto da sala não tem culpa nenhuma disso. Perder uma mensagem
        // e dizê-lo é muito melhor do que perder a sala inteira em silêncio.
        if envelope + n > MAX_FRAME {
            indeliveraveis += 1;
            continue;
        }

        if !atual.is_empty() && tam + n > LOTE_SYNC {
            lotes.push(Msg::Sync {
                servidor: servidor.clone(),
                entradas: std::mem::take(&mut atual),
            });
            tam = 0;
        }
        tam += n;
        atual.push(e);
    }
    if !atual.is_empty() {
        lotes.push(Msg::Sync {
            servidor: servidor.clone(),
            entradas: atual,
        });
    }
    if indeliveraveis > 0 {
        eprintln!(
            "[rede] {indeliveraveis} entrada(s) do servidor {} não cabem num quadro e não vão \
             no sync; o resto da sala segue",
            &servidor[..8.min(servidor.len())]
        );
    }
    // Um servidor sem nada que caiba continua a dizer que é meu.
    if lotes.is_empty() {
        lotes.push(Msg::Sync {
            servidor,
            entradas: Vec::new(),
        });
    }
    lotes
}

/// Escreve um quadro — e se for um `Sync` que não caiba, parte-o em vez de o mandar inteiro.
///
/// Está no caminho de saída, e não só no aperto de mão, porque a retribuição
/// (`Saida::SyncPara`) manda o log completo de uma sala a quem acabou de se provar: é
/// precisamente o caso em que a sala é grande e o outro lado não tem nada.
async fn escrever_quadro_partido(envia: &mut SendStream, q: Quadro) -> Result<()> {
    match q {
        Quadro::Controlo(Msg::Sync { servidor, entradas }) => {
            for m in partir_sync(servidor, entradas) {
                escrever_quadro(envia, &Quadro::Controlo(m)).await?;
            }
            Ok(())
        }
        outro => escrever_quadro(envia, &outro).await,
    }
}

async fn escrever(envia: &mut SendStream, m: &Msg) -> Result<()> {
    escrever_quadro(envia, &Quadro::Controlo(m.clone())).await
}

async fn escrever_quadro(envia: &mut SendStream, q: &Quadro) -> Result<()> {
    let corpo = corpo_do_quadro(q)?;
    // COM PRAZO (#3).
    //
    // Isto eram dois `write_all` sem prazo nenhum. Um par que aceite a ligação e deixe de LER
    // — por avaria, ou de propósito com software modificado — enche a janela do QUIC, e essa
    // espera não termina nunca. A tarefa da sessão fica parada dentro do `select!`, deixa de
    // servir o canal de saída e deixa de vigiar o leitor; e como o `SessaoViva` só limpa no
    // `Drop`, a entrada fica no mapa `ligacoes` para sempre: o vigia vê «já está ligado» e não
    // volta a tentar, a interface conta-o como presente, e a voz continua a ser enviada para
    // ele. É exactamente o estado permanente que o `SessaoViva` foi escrito para impedir,
    // alcançado por outro caminho.
    //
    // O prazo esgotado é fim de sessão: o chamador já trata o erro fechando, e aí o `Drop`
    // faz o resto — o par volta a ser alcançável em vez de ficar preso a fingir.
    let prazo = prazo_de_escrita(q);
    let tam = (corpo.len() as u32).to_be_bytes();
    escrita_com_prazo(envia.write_all(&tam), prazo, "o tamanho").await?;
    escrita_com_prazo(envia.write_all(&corpo), prazo, "o corpo").await?;
    Ok(())
}

/// Os bytes de um quadro, e a recusa de o construir grande de mais.
///
/// À parte do `escrever_quadro` pela razão de sempre neste ficheiro: é uma decisão sobre
/// bytes, e uma decisão testa-se sozinha. O `escrever_quadro` precisa de um `SendStream`,
/// que não se constrói num teste; isto recebe um `Quadro` e devolve um `Vec<u8>`.
fn corpo_do_quadro(q: &Quadro) -> Result<Vec<u8>> {
    let corpo: Vec<u8> = match q {
        // Nunca se ENVIA um desconhecido: a variante existe só para o lado da leitura. Se
        // alguém a construir para enviar, é um bug de programação e não um caso a tolerar.
        Quadro::Desconhecido(porque) => bail!("não se envia um quadro desconhecido ({porque})"),
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
    // NÃO SE ENTREGA UM QUADRO QUE O OUTRO LADO VAI RECUSAR.
    //
    // O tamanho escrevia-se fosse ele qual fosse. Acima do `MAX_FRAME` o receptor faz
    // `bail!` só de ler o cabeçalho — sem consumir o corpo, portanto o stream fica
    // dessincronizado e não há recuperação: a sessão morre, e morre do lado de quem não fez
    // nada de errado, com uma queixa sobre um limite que ele não escolheu.
    //
    // Quem se tem de queixar é quem construiu o quadro. Aqui o erro traz o tipo e o
    // tamanho, e chega antes de um único byte sair. O `Sync` já não passa por aqui grande
    // — vai partido pelo `escrever_quadro_partido` —, portanto isto é o encosto para a
    // PRÓXIMA mensagem que cresça sem ninguém reparar. Que é exactamente como esta nasceu.
    if corpo.len() > MAX_FRAME {
        bail!(
            "quadro {} de {} bytes excede o limite de {MAX_FRAME}: o outro lado recusava-o \n             pelo cabeçalho e a sessão caía — parte-o antes de o mandar",
            match q {
                Quadro::Controlo(m) => nome_da_msg(m),
                Quadro::Video { .. } => "de vídeo",
                Quadro::Desconhecido(_) => "desconhecido",
            },
            corpo.len()
        );
    }
    Ok(corpo)
}

/// Quanto tempo se espera por uma escrita, por tipo de quadro.
///
/// Generoso no controlo e curto no vídeo, e a diferença não é arbitrária: um sync legítimo
/// entre o Brasil e os EUA pode ser grande e lento, e cortá-lo seria partir precisamente o
/// caso de uso; um frame de vídeo que não sai em dois segundos já não interessa a ninguém —
/// quem está a ver quer a imagem de agora, não a de há dois segundos.
fn prazo_de_escrita(q: &Quadro) -> std::time::Duration {
    match q {
        Quadro::Video { .. } => std::time::Duration::from_secs(2),
        _ => std::time::Duration::from_secs(20),
    }
}

/// Corre uma escrita com prazo, e transforma o esgotamento num erro que se percebe.
async fn escrita_com_prazo<F, E>(f: F, prazo: std::time::Duration, o_que: &str) -> Result<()>
where
    F: std::future::Future<Output = std::result::Result<(), E>>,
    E: std::fmt::Display,
{
    match tokio::time::timeout(prazo, f).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(anyhow!("write: {e}")),
        Err(_) => Err(anyhow!(
            "o outro lado deixou de ler: {o_que} não saiu em {prazo:?}"
        )),
    }
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

    interpretar(&corpo)
}

/// O que um corpo de quadro QUER DIZER — sem I/O nenhum.
///
/// Está à parte do `ler()` de propósito: é uma DECISÃO, e uma decisão testa-se sozinha. O
/// `ler()` recebe um stream QUIC, que não se constrói num teste; isto recebe bytes.
///
/// O QUE NÃO SE ENTENDE IGNORA-SE; SÓ O QUE NÃO SE CONSEGUE LER É QUE É ERRO.
///
/// Isto devolvia `Err` para um tipo de quadro desconhecido e para um corpo que não
/// desserializa — e o leitor faz `break` a qualquer `Err`, ou seja, mata a sessão. O
/// `Msg::Desconhecida` existe com um comentário longo a explicar que uma variante nova não
/// pode derrubar a ligação; só que essa tolerância está uma camada ACIMA de onde a decisão é
/// tomada. No dia em que a v0.18 acrescentar um `TIPO_ANEXO`, a primeira coisa que ela
/// mandasse a uma v0.17 matava a ligação — e o vigia religava, e ela voltava a mandar: um
/// ciclo eterno com o sintoma «aparece ligado e não chega nada».
///
/// O enquadramento leva o tamanho à frente, portanto um quadro desconhecido é trivialmente
/// saltável: o corpo já está em memória e deita-se fora. Os erros de ENQUADRAMENTO (não
/// conseguir ler, tamanho absurdo) ficam no `ler()` e continuam a ser erro, porque aí não se
/// sabe onde começa o quadro seguinte — a diferença é entre não perceber o conteúdo e não
/// saber onde ele acaba.
fn interpretar(corpo: &[u8]) -> Result<Quadro> {
    if corpo.is_empty() {
        return Ok(Quadro::Desconhecido("quadro vazio"));
    }
    match corpo[0] {
        TIPO_CONTROLO => match serde_json::from_slice(&corpo[1..]) {
            Ok(m) => Ok(Quadro::Controlo(m)),
            Err(_) => Ok(Quadro::Desconhecido("controlo que não desserializa")),
        },
        TIPO_VIDEO => {
            if corpo.len() < 3 {
                return Ok(Quadro::Desconhecido("vídeo truncado"));
            }
            let tam_cab = u16::from_be_bytes([corpo[1], corpo[2]]) as usize;
            let fim = 3 + tam_cab;
            if corpo.len() < fim {
                return Ok(Quadro::Desconhecido("cabeçalho de vídeo truncado"));
            }
            match serde_json::from_slice::<CabecalhoVideo>(&corpo[3..fim]) {
                Ok(cab) => Ok(Quadro::Video {
                    tipo: cab.tipo,
                    servidor: cab.servidor,
                    canal: cab.canal,
                    dados: corpo[fim..].to_vec(),
                }),
                Err(_) => Ok(Quadro::Desconhecido("cabeçalho de vídeo ilegível")),
            }
        }
        _ => Ok(Quadro::Desconhecido(
            "tipo de quadro que esta versão não conhece",
        )),
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    /// Uma entrada de log de tamanho à escolha, para medir a partição do `Sync`.
    ///
    /// Os campos vão em hexadecimal e o `ciphertext` é o único que cresce — é ele que leva a
    /// mensagem cifrada. Uma mensagem no limite dos 4000 caracteres ocupa dezenas de KiB
    /// depois de cifrada e passada a hexadecimal, portanto é esse o tamanho que interessa
    /// exercitar, e não o de uma linha de conversa.
    fn entrada_de(bytes_de_texto: usize, marca: u8) -> blog::Entry {
        blog::Entry {
            author: "aa".repeat(32),
            ts_ms: 1,
            prev: "bb".repeat(32),
            nonce: "cc".repeat(24),
            ciphertext: format!("{:02x}", marca).repeat(bytes_de_texto.max(1)),
            sig: "dd".repeat(64),
        }
    }

    fn tamanho_do_sync(m: &Msg) -> usize {
        corpo_do_quadro(&Quadro::Controlo(m.clone()))
            .expect("um lote tem de caber, é para isso que ele existe")
            .len()
    }

    /// UM `SYNC` NUNCA VAI GRANDE DE MAIS — e nenhuma entrada se perde a caminho.
    ///
    /// # A avaria que isto mede
    ///
    /// O `Sync` levava o log inteiro de uma sala num só quadro. Acima do `MAX_FRAME` o outro
    /// lado recusa-o pelo cabeçalho, **sem chegar a ler o corpo**: o leitor sai, a sessão
    /// morre, e a tentativa seguinte manda exactamente o mesmo, porque o histórico não
    /// encolhe. Uma sala com umas 14 mil mensagens deixava de poder sincronizar — para
    /// sempre, e com o sintoma «aparece ligado e não chega nada».
    ///
    /// Mede-se o que interessa, que são duas coisas ao mesmo tempo: que **cada** lote cabe
    /// num quadro, e que os lotes juntos são **exactamente** o que se lhes deu, pela mesma
    /// ordem. Partir sem perder é o requisito todo; um dos dois sozinho não prova nada.
    #[test]
    fn o_sync_parte_se_em_lotes_que_cabem_e_nao_perde_nada() {
        // ~40 KiB por entrada e 400 entradas: ~16 MiB, o dobro do MAX_FRAME. Num só quadro
        // isto era recusado pelo outro lado.
        let entradas: Vec<blog::Entry> = (0..400).map(|i| entrada_de(20_000, i as u8)).collect();
        let inteiro = serde_json::to_vec(&Msg::Sync {
            servidor: "s".into(),
            entradas: entradas.clone(),
        })
        .unwrap()
        .len();
        assert!(
            inteiro > MAX_FRAME,
            "o caso de teste tem de ser MAIOR do que o limite, senão não mede nada ({inteiro} bytes)"
        );

        let lotes = partir_sync("s".into(), entradas.clone());
        assert!(lotes.len() > 1, "com {inteiro} bytes tinha de partir");

        let mut juntas: Vec<blog::Entry> = Vec::new();
        for m in &lotes {
            let n = tamanho_do_sync(m);
            assert!(
                n <= MAX_FRAME,
                "um lote de {n} bytes ainda excede o limite: o outro lado recusá-lo-ia na mesma"
            );
            match m {
                Msg::Sync { servidor, entradas } => {
                    assert_eq!(servidor, "s", "o lote mudou de servidor");
                    assert!(
                        !entradas.is_empty(),
                        "um lote vazio no meio não serve para nada"
                    );
                    juntas.extend(entradas.iter().cloned());
                }
                outro => panic!("partir um Sync tem de dar Syncs, deu {outro:?}"),
            }
        }

        assert_eq!(
            juntas.len(),
            entradas.len(),
            "perderam-se ou duplicaram-se entradas"
        );
        for (i, (a, b)) in juntas.iter().zip(entradas.iter()).enumerate() {
            assert_eq!(
                a.ciphertext, b.ciphertext,
                "a entrada {i} mudou ou trocou de lugar"
            );
        }
    }

    /// Uma entrada sozinha maior do que o lote vai à mesma — sozinha, e não deitada fora.
    ///
    /// O empacotamento fecha o lote ANTES de acrescentar a entrada que não cabe. Se essa
    /// verificação não olhasse para o lote já estar vazio, uma entrada grande de mais entrava
    /// num ciclo de lotes vazios ou desaparecia sem aviso — e desaparecer é a pior das duas,
    /// porque é uma mensagem que nunca mais chega e ninguém sabe porquê.
    #[test]
    fn uma_entrada_maior_do_que_o_lote_vai_sozinha_e_nao_se_perde() {
        let entradas = vec![
            entrada_de(10, 1),
            entrada_de(LOTE_SYNC, 2), // sozinha já passa o tecto do lote
            entrada_de(10, 3),
        ];
        let lotes = partir_sync("s".into(), entradas.clone());
        let juntas: Vec<blog::Entry> = lotes
            .iter()
            .flat_map(|m| match m {
                Msg::Sync { entradas, .. } => entradas.clone(),
                _ => Vec::new(),
            })
            .collect();
        assert_eq!(juntas.len(), 3, "a entrada grande não pode desaparecer");
        assert_eq!(juntas[1].ciphertext, entradas[1].ciphertext);
    }

    /// O CONTRATO DO `partir_sync`: nenhum lote sai grande de mais. Nem com uma entrada gigante.
    ///
    /// # A avaria que isto mede
    ///
    /// O empacotamento fecha o lote antes de uma entrada grande, portanto ela acaba **sozinha**
    /// no seu — e um lote de uma entrada só tem o tamanho dessa entrada. O teste dos lotes usa
    /// entradas de ~40 KiB e nunca chega perto do `MAX_FRAME`, portanto passava a afirmar uma
    /// garantia que a função não dava.
    ///
    /// Com uma entrada acima do `MAX_FRAME` no log, o `corpo_do_quadro` recusava-se a construir
    /// o quadro, o `?` do aperto de mão propagava, e a sessão morria antes da `Presenca` e antes
    /// do `select!`. Como o histórico não encolhe, morria outra vez a cada religação: o #213 a
    /// voltar por uma porta mais estreita.
    ///
    /// Que uma entrada assim exista é possível — o `merge_verificado` guarda tudo o que decifra
    /// e assina, sem tecto — e um `Msg::Nova` cabe em três bytes a menos do que o `Msg::Sync`
    /// equivalente, portanto há uma janela em que uma entrada ENTRA e depois não SAI.
    #[test]
    fn nenhum_lote_sai_grande_de_mais_nem_com_uma_entrada_gigante() {
        let gigante = entrada_de(MAX_FRAME, 9);
        assert!(
            serde_json::to_vec(&gigante).unwrap().len() > MAX_FRAME,
            "o caso tem de ser mesmo maior do que um quadro"
        );
        let entradas = vec![entrada_de(10, 1), gigante.clone(), entrada_de(10, 3)];

        let lotes = partir_sync("s".into(), entradas);
        for m in &lotes {
            // O `tamanho_do_sync` já faz `expect` no `corpo_do_quadro`: se um lote não coubesse,
            // rebentava aqui — que é exactamente o que acontecia na sessão.
            let n = tamanho_do_sync(m);
            assert!(n <= MAX_FRAME, "um lote de {n} bytes não cabe num quadro");
        }

        let juntas: Vec<blog::Entry> = lotes
            .iter()
            .flat_map(|m| match m {
                Msg::Sync { entradas, .. } => entradas.clone(),
                _ => Vec::new(),
            })
            .collect();
        assert_eq!(juntas.len(), 2, "as duas que cabem têm de ir, e só elas");
        assert!(
            !juntas.iter().any(|e| e.ciphertext == gigante.ciphertext),
            "a entrada indeliverável não pode ir dentro de um lote"
        );
    }

    /// E uma sala onde NADA cabe continua a dizer que é minha.
    ///
    /// Sem isto, um servidor cujas entradas fossem todas indeliveráveis desaparecia do sync: o
    /// outro lado nunca sabia que a sala existe, e a retribuição nunca acontecia. Um `Sync`
    /// vazio custa nada e mantém a sala no mapa dos dois.
    #[test]
    fn uma_sala_so_com_entradas_indeliveraveis_ainda_se_anuncia() {
        let lotes = partir_sync("s".into(), vec![entrada_de(MAX_FRAME, 9)]);
        assert_eq!(lotes.len(), 1);
        match &lotes[0] {
            Msg::Sync { servidor, entradas } => {
                assert_eq!(servidor, "s");
                assert!(entradas.is_empty(), "nada cabia, portanto o lote é vazio");
            }
            outro => panic!("devia ser um Sync vazio, deu {outro:?}"),
        }
    }

    /// Um `Sync` VAZIO continua a ir.
    ///
    /// É ele que diz «este servidor é meu e não tenho nada para te dar». Uma partição ingénua
    /// devolvia zero lotes para uma lista vazia e calava-o — e com ele calava o simulador de
    /// ataque, que precisa de o poder mandar para provar que o porteiro não inscreve ninguém
    /// só por dizer o nome de um servidor.
    #[test]
    fn um_sync_vazio_continua_a_ir() {
        let lotes = partir_sync("s".into(), Vec::new());
        assert_eq!(lotes.len(), 1, "um Sync vazio não pode ser calado");
        match &lotes[0] {
            Msg::Sync { servidor, entradas } => {
                assert_eq!(servidor, "s");
                assert!(entradas.is_empty());
            }
            outro => panic!("devia ser um Sync vazio, deu {outro:?}"),
        }
    }

    /// O ENVIO recusa-se a construir um quadro que o outro lado ia recusar.
    ///
    /// O tamanho escrevia-se fosse ele qual fosse, e a queixa aparecia do lado errado: quem
    /// recebia via «quadro de N bytes excede o limite» e morria, sem ter feito nada. Aqui o
    /// erro chega a quem o construiu, antes de um único byte sair, e diz QUAL mensagem
    /// cresceu — que é o que se procura no código a seguir.
    #[test]
    fn nao_se_constroi_um_quadro_grande_de_mais() {
        let pequeno = Quadro::Controlo(Msg::Sync {
            servidor: "s".into(),
            entradas: vec![entrada_de(10, 1)],
        });
        assert!(
            corpo_do_quadro(&pequeno).is_ok(),
            "um quadro normal tem de continuar a passar"
        );

        let grande = Quadro::Controlo(Msg::Sync {
            servidor: "s".into(),
            entradas: vec![entrada_de(MAX_FRAME, 2)],
        });
        // O `expect_err` imprimia o `Ok` inteiro quando o guarda saísse — oito megabytes de
        // bytes no ecrã, para dizer uma frase. Um teste que falha tem de ser legível.
        let erro = match corpo_do_quadro(&grande) {
            Err(e) => e.to_string(),
            Ok(v) => panic!(
                "um quadro de {} bytes foi construído; o MAX_FRAME é {MAX_FRAME}",
                v.len()
            ),
        };
        assert!(
            erro.contains("Sync"),
            "a queixa tem de dizer QUAL mensagem cresceu, e disse: {erro}"
        );
    }

    /// ATENDER NÃO É FUNCIONAR: discar não limpa o recuo, nem quando o outro lado atende.
    ///
    /// # A avaria que isto mede
    ///
    /// O recuo limpava-se no ramo `Ok` do `ligar`. Mas o `ligar` devolve `Ok` assim que faz
    /// `spawn` da sessão — antes de um único byte ir ao fio. Se a sessão morresse logo a
    /// seguir (um `Sync` grande de mais, um duelo de ligações, um par que bloqueámos), o
    /// `Drop` tirava-a do mapa, dois segundos depois o vigia via que não estava ligado,
    /// discava, recebia `Ok`, e limpava o recuo outra vez. **Ciclo de dois em dois segundos,
    /// para sempre** — a dizer ao outro lado, com o próprio tráfego, que estamos online.
    ///
    /// A regra que substitui aquilo: só a PROVA de uma sessão viva (estar no mapa das
    /// ligações, que é o que o `pegou` representa) limpa o recuo.
    #[test]
    fn atender_nao_limpa_o_recuo_e_so_uma_sessao_viva_o_faz() {
        let mut a = Adiamento::default();
        let t0 = std::time::Instant::now();
        let seg = std::time::Duration::from_secs(1);

        // Primeira tentativa: agenda daqui a 2 s.
        assert_eq!(a.discou("p", t0), 2);
        assert!(a.ainda_cedo("p", t0 + seg), "1 s depois ainda é cedo");
        assert!(
            !a.ainda_cedo("p", t0 + 2 * seg),
            "aos 2 s já se pode tentar"
        );

        // A sessão atendeu e morreu — nunca houve `pegou`. O recuo TEM de crescer.
        assert_eq!(
            a.discou("p", t0 + 2 * seg),
            4,
            "atender não pode repor o recuo a 2"
        );
        assert_eq!(a.discou("p", t0 + 6 * seg), 8);
        assert_eq!(a.discou("p", t0 + 14 * seg), 16);

        // E tem tecto: não cresce até ao infinito.
        for _ in 0..10 {
            a.discou("p", t0);
        }
        assert_eq!(a.discou("p", t0), 60, "o recuo tem de parar nos 60 s");

        // Prova de sessão viva: aí sim, volta ao princípio. Uma ligação que caia ao fim de
        // uma hora de conversa tem de ser tentada daqui a 2 s, e não daqui a um minuto.
        a.pegou("p");
        assert!(
            !a.ainda_cedo("p", t0),
            "depois de pegar não há nada a adiar"
        );
        assert_eq!(a.discou("p", t0), 2, "o recuo tem de voltar ao princípio");
    }

    /// E cada par tem o seu recuo: o de um não adia o outro.
    #[test]
    fn o_recuo_e_por_par() {
        let mut a = Adiamento::default();
        let t0 = std::time::Instant::now();
        a.discou("p1", t0);
        a.discou("p1", t0);
        assert_eq!(
            a.discou("p2", t0),
            2,
            "o p2 nunca falhou, não pode herdar o recuo do p1"
        );
    }

    /// Um `App` de teste com uma sala e um membro — o mínimo para exercitar os porteiros.
    ///
    /// Os porteiros lêem só o mapa de servidores; não precisam de rede, nem de disco, nem de
    /// identidade. É por isso que se testam aqui, e é por isso que valem: a decisão é uma
    /// função do estado, e uma função do estado prova-se.
    fn app_com_sala(membros: &[&str]) -> Arc<App> {
        let app = crate::estado::App::para_teste();
        {
            let mut s = app.servidores.lock().unwrap();
            let mut srv = crate::estado::Servidor::para_teste("sala");
            srv.peers = membros.iter().map(|m| m.to_string()).collect();
            s.insert("sala".to_string(), srv);
        }
        app
    }

    /// A VOZ SÓ SAI PARA QUEM PERTENCE À SALA (#138).
    ///
    /// # A avaria que isto mede
    ///
    /// A lista de quem me ouve vivia só no JavaScript, alimentada por mensagens de presença
    /// que chegam da rede. Um par com software modificado que forjasse presença entrava nessa
    /// lista, e o Rust escrevia-lhe datagramas sem perguntar nada — cinquenta vezes por
    /// segundo, com o meu microfone lá dentro.
    ///
    /// A presença, o vídeo e a sincronização já passavam pelo `participa`. A voz não. Este
    /// teste é a afirmação de que passa.
    #[test]
    fn a_voz_so_sai_para_quem_pertence_a_sala() {
        let de_casa = "aa".repeat(32);
        let forjado = "ff".repeat(32);
        let app = app_com_sala(&[&de_casa]);

        let pedido = vec![de_casa.clone(), forjado.clone()];
        let permitidos = so_quem_participa(&app, "sala", &pedido);
        assert_eq!(permitidos, vec![de_casa.clone()], "só o membro recebe voz");

        // E numa sala que nem sequer existe, não sai nada para ninguém.
        assert!(
            so_quem_participa(&app, "sala-que-nao-tenho", &pedido).is_empty(),
            "uma sala que não é minha não autoriza ninguém"
        );
    }

    /// «DE CASA» é partilhar uma sala ou ter sido convidado por mim (#195, #136).
    ///
    /// É este o critério que decide a quem se diz o nome e quem não conta para o tecto de
    /// desconhecidos. Uma CONVERSA não conta — qualquer pessoa com a minha chave pública abre
    /// uma comigo, e a chave é pública por desenho: se contasse, bastava escrever-me para
    /// deixar de ser estranho.
    #[test]
    fn de_casa_e_partilhar_sala_ou_ter_sido_convidado() {
        let membro = "aa".repeat(32);
        let estranho = "ff".repeat(32);
        let app = app_com_sala(&[&membro]);
        assert!(e_de_casa(&app, &membro));
        assert!(!e_de_casa(&app, &estranho));

        // Uma conversa com o estranho NÃO o torna de casa.
        {
            let mut s = app.servidores.lock().unwrap();
            let mut conversa = crate::estado::Servidor::para_teste("conversa");
            conversa.peers = vec![estranho.clone()];
            conversa.com = Some(estranho.clone());
            s.insert("conversa".to_string(), conversa);
        }
        assert!(
            !e_de_casa(&app, &estranho),
            "abrir-me uma conversa não pode dar direitos de sala"
        );

        // Mas ter sido convidado por mim, sim: é o único fio antes de haver prova.
        {
            let mut s = app.servidores.lock().unwrap();
            s.get_mut("sala").unwrap().convidou = Some(estranho.clone());
        }
        assert!(e_de_casa(&app, &estranho), "quem eu convidei é de casa");
    }

    /// O TECTO DE DESCONHECIDOS NUNCA CORTA GENTE DE CASA (#195).
    ///
    /// # A avaria que isto mede
    ///
    /// O ciclo de `accept` aceitava qualquer ligação com o ALPN certo e lançava uma tarefa por
    /// cada uma, sem limite. Quem tem a minha chave — que viaja em claro em qualquer convite
    /// reencaminhado — podia abrir quantas quisesse.
    ///
    /// Mas um tecto mal feito é pior do que nenhum: cortar um par legítimo é uma avaria que
    /// parece rede e não se diagnostica. Por isso a primeira coisa que este teste afirma não é
    /// que o tecto corta — é que ele NÃO corta quem é de casa, esteja a casa como estiver.
    #[test]
    fn o_tecto_de_estranhos_nunca_corta_gente_de_casa() {
        let membro = "aa".repeat(32);
        let app = app_com_sala(&[&membro]);
        let estranhos: Vec<String> = (0..40u8).map(|i| format!("{:02x}", i).repeat(32)).collect();

        // A casa a abarrotar de desconhecidos, e o membro chega: entra.
        assert!(
            !ha_estranhos_a_mais(&app, &estranhos, &membro),
            "quem partilha sala comigo entra sempre"
        );

        // Abaixo do tecto, um desconhecido também entra.
        let poucos = estranhos[..TECTO_DE_ESTRANHOS].to_vec();
        assert!(
            !ha_estranhos_a_mais(&app, &poucos, &estranhos[9]),
            "com {TECTO_DE_ESTRANHOS} ligados ainda cabe mais um"
        );

        // Acima, não. (O `ligados` já inclui quem chega: é a sessão que acabou de se
        // registar no mapa.)
        let demais = estranhos[..TECTO_DE_ESTRANHOS + 1].to_vec();
        assert!(
            ha_estranhos_a_mais(&app, &demais, &estranhos[0]),
            "acima do tecto, um desconhecido é recusado"
        );

        // E os de casa não gastam lugar: quarenta membros ligados não fecham a porta.
        let cheia = app_com_sala(&estranhos.iter().map(|s| s.as_str()).collect::<Vec<_>>());
        assert!(
            !ha_estranhos_a_mais(&cheia, &estranhos, &estranhos[0]),
            "quarenta pares de casa não contam para o tecto dos desconhecidos"
        );
    }

    /// O ORÇAMENTO TRAVA O PRIMEIRO LOTE — não o segundo (#85).
    ///
    /// # A avaria que isto mede
    ///
    /// A primeira versão do porteiro comparava só o que JÁ tinha sido gasto. Isso deixava o
    /// primeiro lote de cada sessão passar inteiro, fosse ele qual fosse — e uma religação
    /// repunha a conta a zero, portanto bastava religar para voltar a ter direito a um lote
    /// sem tecto nenhum. O que este porteiro existe para impedir é a app a congelar a
    /// decifrar um quadro cheio de lixo, e o quadro que congela é precisamente o primeiro.
    ///
    /// Agora conta-se o que já se gastou MAIS o pior caso deste lote, que é ele inteiro.
    #[test]
    fn o_orcamento_trava_o_primeiro_lote_e_nao_o_segundo() {
        // A decisão, extraída tal como está no `aplicar`: quem não provou tem tecto, e o
        // tecto olha para o lote que vem a caminho.
        let cabe = |gasto: usize, lote: usize| gasto + lote <= LIXO_TOLERADO;

        assert!(
            !cabe(0, LIXO_TOLERADO + 1),
            "um primeiro lote acima do tecto tem de ser travado ANTES de se decifrar nada"
        );
        assert!(
            cabe(0, LIXO_TOLERADO),
            "e um lote do tamanho do tecto ainda passa"
        );
        assert!(
            !cabe(LIXO_TOLERADO, 1),
            "esgotado o orçamento, nem mais uma entrada"
        );
    }

    /// O TECTO DE DESCONHECIDOS NÃO SE APLICA A QUEM EU DISCO.
    ///
    /// Uma ligação que eu inicio foi decisão minha: o vigia só disca a quem está nos `peers`
    /// de alguma sala minha, ou a um amigo. Aplicar-lhe o tecto seria o porteiro a cortar-me a
    /// mim próprio por azar na ordem das ligações — e o sintoma seria «às vezes não me ligo a
    /// ele», que é a pior espécie de avaria intermitente.
    ///
    /// O teste é sobre a REGRA, não sobre a `sessao`: afirma que um par que eu disco e que já
    /// é de casa nunca é recusado, esteja a casa como estiver. O `if !iniciei` que envolve a
    /// chamada está ao lado, com a razão escrita.
    #[test]
    fn quem_eu_disco_nunca_e_recusado_pelo_tecto() {
        let amigo = "aa".repeat(32);
        let app = app_com_sala(&[&amigo]);
        let multidao: Vec<String> = (0..30u8).map(|i| format!("{:02x}", i).repeat(32)).collect();
        assert!(
            !ha_estranhos_a_mais(&app, &multidao, &amigo),
            "quem partilha sala comigo entra sempre, e é a esses que o vigia disca"
        );
    }

    /// O RITMO É O PRESENTE; O TOTAL É O PASSADO (#33).
    ///
    /// # A avaria que isto mede
    ///
    /// Os contadores só crescem desde que a ligação abriu. O painel marcava «mudo» só quando
    /// `recebidos == 0 && enviados > 0` — ou seja, apanhava a avaria que existiu desde o
    /// primeiro segundo e mais nenhuma. Se a voz da outra pessoa morresse ao minuto dez, o
    /// painel mostrava «↑30000 ↓29000» e parecia saudável para sempre.
    ///
    /// Um total não sabe dizer «agora». O que este teste prende é que o ritmo sabe: depois de
    /// o outro lado se calar, o `rec_s` cai a zero enquanto o `voz_rec` continua grande.
    #[test]
    fn o_ritmo_cai_a_zero_quando_a_voz_para_mesmo_com_o_total_grande() {
        let t0 = std::time::Instant::now();
        // Dez minutos de conversa já feitos: os totais estão altos.
        let mut c = Contagem {
            voz_env: 30_000,
            voz_rec: 29_000,
            ..Default::default()
        };
        c.recalcular_ritmo(t0); // primeira vez: só tira a fotografia

        // Um segundo depois, com cinquenta pacotes em cada sentido.
        c.voz_env += 50;
        c.voz_rec += 50;
        c.recalcular_ritmo(t0 + std::time::Duration::from_secs(1));
        assert_eq!((c.env_s, c.rec_s), (50, 50), "a conversa a decorrer");

        // E agora ele cala-se: eu continuo a mandar, dele não vem nada.
        c.voz_env += 50;
        c.recalcular_ritmo(t0 + std::time::Duration::from_secs(2));
        assert_eq!(c.env_s, 50, "eu continuo a falar");
        assert_eq!(
            c.rec_s, 0,
            "e dele já não vem nada — que é o que o total esconderia"
        );
        assert!(
            c.voz_rec > 29_000,
            "o total continua grande, e continua a parecer saudável"
        );
    }

    /// «Há quanto tempo» só existe depois de ter chegado alguma coisa.
    ///
    /// Zero segundos e «nunca chegou nada» são estados diferentes, e confundi-los é a mesma
    /// família de erro do RTT a zero (#171): um deles é uma medida, o outro é a ausência dela.
    #[test]
    fn ha_quanto_rec_distingue_nunca_de_agora_mesmo() {
        let t0 = std::time::Instant::now();
        let mut c = Contagem::default();
        assert_eq!(
            c.ha_quanto_rec(t0),
            None,
            "nunca chegou nada: não há resposta"
        );

        c.ultimo_rec = Some(t0);
        assert_eq!(c.ha_quanto_rec(t0), Some(0), "acabou de chegar");
        assert_eq!(
            c.ha_quanto_rec(t0 + std::time::Duration::from_secs(90)),
            Some(90_000),
            "e noventa segundos depois, noventa mil milissegundos"
        );
    }

    /// A PERDA SÓ EXISTE COM AS DUAS METADES (#124).
    ///
    /// # A avaria que isto mede
    ///
    /// O receptor conta os pedaços que chegaram e mais nada. Com esse número sozinho não há
    /// como distinguir «ele calou-se» de «perdi trinta pacotes» — e num percurso EUA-Brasil
    /// por relay a segunda hipótese é rotina. A app dizia «Voz conectada» nas duas.
    ///
    /// A metade que faltava é o emissor dizer quantos mandou. Aí a subtracção é a perda, e
    /// passa a haver um número onde havia um palpite.
    ///
    /// E há três casos que NÃO podem virar «0%», que é o erro fácil: ninguém disse nada
    /// ainda, chegaram mais do que os anunciados (o anúncio viaja, e entretanto chegam
    /// mais), e nada se perdeu de facto. O primeiro é ausência de medida e tem de o dizer.
    #[test]
    fn a_perda_precisa_das_duas_metades_e_ausencia_nao_e_zero() {
        // Chegaram 500 e ele ainda não disse nada: não há perda para calcular, e isso NÃO é
        // zero por cento.
        let mut c = Contagem {
            voz_rec: 500,
            ..Default::default()
        };
        assert_eq!(
            c.perda_por_cento(),
            None,
            "sem o anúncio dele não há medida — e ausência de medida não se pinta de bom"
        );

        // Ele diz ter mandado 1000; chegaram 500. Metade perdeu-se.
        c.disse_ter_enviado = 1000;
        assert_eq!(c.perda_por_cento(), Some(50.0));

        // Chegou tudo.
        c.voz_rec = 1000;
        assert_eq!(c.perda_por_cento(), Some(0.0));

        // E o caso do relógio: o anúncio dele viaja, e entretanto chegaram mais. Uma perda
        // «negativa» é o desencontro dos instantes, não a rede — apara-se a zero.
        c.voz_rec = 1200;
        assert_eq!(
            c.perda_por_cento(),
            Some(0.0),
            "mais do que os anunciados é o relógio, não uma perda negativa"
        );
    }

    /// Uma mensagem de uma versao mais nova NAO pode derrubar a ligacao.
    ///
    /// Sem `#[serde(other)]`, o `serde` recusa um `t` que nao conheca, o `ler()` devolve
    /// `Err` e o leitor faz `break`. No dia em que uma versao acrescentasse uma mensagem,
    /// deixava de conseguir falar com todas as anteriores -- e o sintoma nao seria "essa
    /// funcionalidade nao funciona", seria "o outro aparece ligado e nao chega nada".
    /// O `Ola` de uma versão ANTERIOR (sem o campo `versao`) continua a ser lido.
    ///
    /// É o ponto todo de pôr o campo numa variante que já existe, com `Option` e `default`:
    /// uma variante nova derrubaria a ligação com quem ainda não actualizou.
    #[test]
    fn o_ola_sem_versao_continua_a_ser_lido() {
        // O que a v0.17 manda — não conhece o campo `versao`.
        let antigo = br#"{"t":"Ola","nome":"Rakjsu","x_pub":"aa","prekey_sig":"bb"}"#;
        let m: Msg = serde_json::from_slice(antigo).expect("o Olá antigo tem de ser lido");
        match m {
            Msg::Ola { nome, versao, .. } => {
                assert_eq!(nome, "Rakjsu");
                assert!(
                    versao.is_none(),
                    "sem campo é None, e isso também é informação"
                );
            }
            outro => panic!("devia ser um Olá, deu {outro:?}"),
        }

        // E o de uma versão que já o manda.
        let novo = br#"{"t":"Ola","nome":"R","x_pub":"aa","prekey_sig":"bb","versao":"0.18.0"}"#;
        match serde_json::from_slice::<Msg>(novo).unwrap() {
            Msg::Ola { versao, .. } => assert_eq!(versao.as_deref(), Some("0.18.0")),
            outro => panic!("devia ser um Olá, deu {outro:?}"),
        }
    }

    /// O prazo de escrita depende do tipo: generoso no controlo, curto no vídeo.
    ///
    /// Não é arbitrário. Um sync legítimo entre o Brasil e os EUA pode ser grande e lento, e
    /// cortá-lo seria partir o caso de uso; um frame que não sai em dois segundos já não
    /// interessa a ninguém.
    #[test]
    fn o_prazo_de_escrita_distingue_video_de_controlo() {
        let video = Quadro::Video {
            tipo: "ecra".into(),
            servidor: "s".into(),
            canal: "c".into(),
            dados: vec![],
        };
        let controlo = Quadro::Controlo(Msg::Desconhecida);
        let pv = prazo_de_escrita(&video);
        let pc = prazo_de_escrita(&controlo);
        assert!(
            pv < pc,
            "o vídeo tem de ter prazo mais curto: {pv:?} vs {pc:?}"
        );
        assert!(
            pv >= std::time::Duration::from_secs(1),
            "mas não tão curto que corte um frame normal"
        );
        assert!(
            pc >= std::time::Duration::from_secs(10),
            "e o controlo tem de aguentar um sync grande e lento: {pc:?}"
        );
    }

    /// Uma escrita que nunca acaba dá erro, em vez de prender a sessão para sempre.
    #[tokio::test]
    async fn uma_escrita_que_nunca_acaba_da_erro() {
        // Um futuro que nunca resolve — é o que um `write_all` faz quando o outro lado deixa
        // de ler e a janela do QUIC enche.
        let nunca = std::future::pending::<std::result::Result<(), std::io::Error>>();
        let r = escrita_com_prazo(nunca, std::time::Duration::from_millis(50), "o corpo").await;
        let e = r.expect_err("uma escrita presa tinha de dar erro");
        assert!(
            e.to_string().contains("deixou de ler"),
            "e o erro tem de dizer o que se passou: {e}"
        );

        // E uma escrita que acaba a tempo passa — senão trocava-se um bloqueio por uma
        // sessão que morre sozinha.
        let pronta = std::future::ready::<std::result::Result<(), std::io::Error>>(Ok(()));
        assert!(
            escrita_com_prazo(pronta, std::time::Duration::from_secs(5), "o corpo")
                .await
                .is_ok(),
            "uma escrita normal não pode falhar"
        );
    }

    /// Um QUADRO de um tipo que esta versão não conhece não pode derrubar a ligação.
    ///
    /// É o irmão do teste abaixo, uma camada mais abaixo — e é a camada que decide primeiro.
    /// A tolerância do `Msg::Desconhecida` não servia de nada enquanto o enquadramento
    /// matasse a sessão antes de o JSON sequer ser lido.
    #[test]
    fn um_quadro_desconhecido_nao_derruba_a_sessao() {
        // O que a v0.18 mandaria: um tipo que ainda não existe, com um corpo qualquer.
        let mut futuro = vec![7u8];
        futuro.extend_from_slice(b"o que quer que a proxima versao invente");
        let q = interpretar(&futuro).expect("um tipo desconhecido NÃO pode ser erro");
        assert!(
            matches!(q, Quadro::Desconhecido(_)),
            "devia cair no desconhecido, deu outra coisa"
        );

        // Um corpo de um tipo CONHECIDO que não desserializa: idem. É o mesmo `break` do
        // leitor, chegado por outro caminho.
        let mut lixo = vec![TIPO_CONTROLO];
        lixo.extend_from_slice(b"{isto nao e json");
        assert!(
            matches!(interpretar(&lixo), Ok(Quadro::Desconhecido(_))),
            "um controlo ilegível não pode matar a sessão"
        );

        // E a tolerância NÃO pode engolir o que se conhece — senão troca-se uma ligação
        // partida por uma app muda, que é pior.
        let mut ola = vec![TIPO_CONTROLO];
        ola.extend_from_slice(br#"{"t":"Ola","nome":"Rakjsu"}"#);
        assert!(
            matches!(interpretar(&ola), Ok(Quadro::Controlo(Msg::Ola { .. }))),
            "o Olá deixou de ser lido"
        );
    }

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

    /// O SYNC DE UMA SALA GRANDE ATRAVESSA MESMO O FIO — e é este o defeito, inteiro.
    ///
    /// # A avaria que isto mede
    ///
    /// Os testes acima medem a partição como decisão: dão-lhe entradas e olham para os lotes.
    /// Nenhum deles põe um byte no fio, e é no fio que o defeito vivia — o `ler()` recusa um
    /// quadro pelo CABEÇALHO, sem sequer consumir o corpo, portanto o stream fica
    /// dessincronizado e a sessão morre. Não há forma de descobrir isso a olhar para uma
    /// função pura.
    ///
    /// Aqui monta-se o caso a sério: dois endpoints, um stream, e um `Sync` de mais de 8 MiB
    /// — o histórico de uma sala com uns milhares de mensagens. Antes desta correcção, este
    /// teste morria na primeira leitura, com «quadro de N bytes excede o limite». E morria
    /// **outra vez** a cada religação, porque o histórico não encolhe: era o ciclo.
    ///
    /// Exige-se três coisas ao mesmo tempo, e as três são precisas: que TENHA sido partido
    /// (senão não estamos a medir o caso), que cada quadro tenha sido lido sem erro, e que as
    /// entradas cheguem TODAS e pela mesma ordem — partir sem perder é o requisito.
    #[tokio::test]
    async fn um_sync_maior_do_que_o_quadro_atravessa_o_fio_em_lotes() {
        // ~40 KiB por entrada, 260 entradas: ~10 MiB, acima dos 8 MiB do MAX_FRAME.
        let entradas: Vec<blog::Entry> = (0..260).map(|i| entrada_de(20_000, i as u8)).collect();
        let quantas = entradas.len();
        let inteiro = serde_json::to_vec(&Msg::Sync {
            servidor: "sala-grande".into(),
            entradas: entradas.clone(),
        })
        .unwrap()
        .len();
        assert!(
            inteiro > MAX_FRAME,
            "o caso tem de ser maior do que um quadro, senão não mede o defeito ({inteiro} bytes)"
        );

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
            let conn = b
                .accept()
                .await
                .expect("ligação a chegar")
                .await
                .expect("aceitar");
            let (_envia, mut recebe) = conn.accept_bi().await.expect("stream");
            let mut recebidas: Vec<blog::Entry> = Vec::new();
            let mut quadros = 0usize;
            while recebidas.len() < quantas {
                // Um `Err` aqui É o defeito: é exactamente assim que a sessão morria.
                match ler(&mut recebe).await.expect("um quadro tem de se ler") {
                    Quadro::Controlo(Msg::Sync { entradas, .. }) => {
                        quadros += 1;
                        recebidas.extend(entradas);
                    }
                    outro => panic!(
                        "esperava um Sync, veio um quadro {}",
                        match &outro {
                            Quadro::Controlo(m) => nome_da_msg(m),
                            Quadro::Video { .. } => "de vídeo",
                            Quadro::Desconhecido(porque) => porque,
                        }
                    ),
                }
            }
            (recebidas, quadros)
        });

        let conn = a.connect(endereco_b, ALPN).await.expect("ligar");
        let (mut envia, _recebe) = conn.open_bi().await.expect("stream");
        escrever_quadro_partido(
            &mut envia,
            Quadro::Controlo(Msg::Sync {
                servidor: "sala-grande".into(),
                entradas: entradas.clone(),
            }),
        )
        .await
        .expect("um Sync grande tem de poder ser enviado");

        let (recebidas, quadros) =
            tokio::time::timeout(std::time::Duration::from_secs(60), ouvinte)
                .await
                .expect("o sync não atravessou a tempo")
                .expect("tarefa do ouvinte");

        assert!(
            quadros > 1,
            "veio num quadro só ({quadros}): ou não partiu, ou o caso encolheu"
        );
        assert_eq!(
            recebidas.len(),
            quantas,
            "chegaram menos entradas do que se mandou"
        );
        for (i, (x, y)) in recebidas.iter().zip(entradas.iter()).enumerate() {
            assert_eq!(
                x.ciphertext, y.ciphertext,
                "a entrada {i} trocou de lugar ou mudou"
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

    /// O corte de vídeo do #114 não pode ser um ferrolho de sentido único.
    ///
    /// Esta é a propriedade que o teste de par **não** consegue isolar: lá há sempre outro
    /// tráfego — presença, sync, a conversa privada — cujas escritas voltam a medir e
    /// destapam o corte por acidente. Sabotei a validade e o par recuperou na mesma, o que
    /// diz que aquela medição não estava a provar isto. Aqui prova.
    #[test]
    fn o_corte_de_video_destranca_se_sozinho() {
        let agora = std::time::Instant::now();
        let mut c = Contagem::default();

        // Nunca se escreveu nada: não há motivo para cortar.
        assert!(!corta_video(&c, agora), "sem medida nenhuma não se corta");

        // Uma escrita lenta agora mesmo: corta-se, que é o comportamento que se quer.
        c.escrita_ultima_ms = ESCRITA_LENTA_MS + 1;
        c.escrita_medida_em = Some(agora);
        assert!(
            corta_video(&c, agora),
            "uma escrita lenta recente tem de cortar"
        );

        // A MESMA medida, passada a validade: já não decide nada. É este o passo que
        // faltava — sem ele, o valor acima ficava a cortar para sempre, porque cortar é
        // precisamente o que impede a escrita seguinte de o actualizar.
        let depois = agora + VALIDADE_DA_MEDIDA + std::time::Duration::from_millis(1);
        assert!(
            !corta_video(&c, depois),
            "uma medida caducada não pode continuar a cortar: é o ferrolho"
        );

        // E uma escrita rápida não corta, esteja fresca ou velha.
        c.escrita_ultima_ms = ESCRITA_LENTA_MS;
        c.escrita_medida_em = Some(agora);
        assert!(!corta_video(&c, agora), "no limiar ainda não se corta");
    }

    /// O caminho POR RELAY, que o README promete e que não tinha um único teste (#126).
    ///
    /// # Porque é que isto estava a descoberto
    ///
    /// O teste acima usa `presets::N0DisableRelay` — desliga explicitamente o relay. Prova a
    /// ligação directa, que é o caso bom. O README diz «se o router não se deixar furar, a
    /// ligação passa por um relay em vez de falhar», e a barra da chamada di-lo — e esse
    /// caminho, o do caso MAU, que é precisamente aquele em que se vai cair entre os EUA e o
    /// Brasil quando o furo falhar, não tinha teste, nem bandeira, nem medição.
    ///
    /// # Porquê `#[ignore]`
    ///
    /// O relay público do n0 é infraestrutura de terceiros. Um teste que dependa dela no CI
    /// fica intermitente, e um vermelho intermitente ensina-se a ignorar — o que é pior do
    /// que não ter teste nenhum, porque o vermelho a seguir também será ignorado.
    ///
    /// Corre-se de propósito: `cargo test -p bruma -- --ignored atravessa_pelo_relay`.
    #[tokio::test]
    #[ignore = "depende do relay público do n0; corre-se à mão"]
    async fn um_datagrama_atravessa_pelo_relay() {
        use iroh::Watcher;

        // O `N0` traz o relay; e força-se o caminho a passar por ele não dando ao outro lado
        // nenhum endereço IP — só o do relay. É a situação de quem está atrás de um NAT que
        // não se deixa furar, sem precisar de um NAT.
        let a = Endpoint::builder(presets::N0)
            .alpns(vec![ALPN.to_vec()])
            .bind()
            .await
            .expect("endpoint A");
        let b = Endpoint::builder(presets::N0)
            .alpns(vec![ALPN.to_vec()])
            .bind()
            .await
            .expect("endpoint B");

        // Sem relay ligado dos dois lados não há caminho nenhum a testar, e o teste diria
        // «passou» sobre uma ligação directa. Espera-se por ele, com tecto.
        for (nome, ep) in [("A", &a), ("B", &b)] {
            tokio::time::timeout(std::time::Duration::from_secs(20), ep.online())
                .await
                .unwrap_or_else(|_| panic!("{nome} não conseguiu relay em 20 s"));
        }

        // O endereço que se dá ao A é só o relay do B: sem IPs, o único caminho possível é
        // por lá.
        let relay_de_b = b
            .home_relay_status()
            .get()
            .into_iter()
            .find(|r| r.is_connected())
            .map(|r| r.url().clone())
            .expect("B devia ter um relay ligado");
        let endereco_b = EndpointAddr::from_parts(b.id(), [iroh::TransportAddr::Relay(relay_de_b)]);

        let ouvinte = tokio::spawn(async move {
            let incoming = b.accept().await.expect("ligação a chegar");
            let conn = incoming.await.expect("aceitar");
            conn.read_datagram().await.expect("datagrama")
        });

        let conn = a.connect(endereco_b, ALPN).await.expect("ligar pelo relay");

        // E confirma-se que foi MESMO pelo relay: um teste que passasse por ter caído numa
        // ligação directa provaria outra vez o que o teste de cima já prova.
        let pelo_relay = conn
            .paths()
            .iter()
            .find(|p| p.is_selected())
            .map(|p| p.is_relay())
            .unwrap_or(false);
        assert!(
            pelo_relay,
            "o caminho escolhido não é o relay: este teste passaria pela razão errada"
        );

        assert!(
            conn.max_datagram_size().is_some(),
            "o par tem de aceitar datagramas mesmo por relay, senão a voz não passa"
        );
        conn.send_datagram(bytes::Bytes::from_static("ola pelo relay".as_bytes()))
            .expect("enviar");

        let recebido = tokio::time::timeout(std::time::Duration::from_secs(20), ouvinte)
            .await
            .expect("não chegou a tempo pelo relay")
            .expect("tarefa");
        assert_eq!(&recebido[..], b"ola pelo relay");
    }
}
