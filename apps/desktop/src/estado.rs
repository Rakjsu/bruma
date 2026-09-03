//! Identidade, armazenamento e o estado vivo da aplicação.
//!
//! A identidade é uma semente de 32 bytes e mais nada. Dela sai a chave Ed25519 que é
//! simultaneamente o ID da pessoa e o endereço dela na rede — não há conta, não há e-mail,
//! não há servidor que registe seja o que for.
//!
//! Cada servidor tem uma chave simétrica própria, criada por quem o fundou e distribuída
//! dentro do convite. Isso significa que **o convite é um segredo**, não um endereço.

use anyhow::{anyhow, bail, Result};
use data_encoding::HEXLOWER;
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use spike_common::{crypto, log as blog};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::modelo::{self, Aplicavel, Carga, EstadoDoServidor, MensagemVista};

/// Onde vive tudo. Fica ao lado do executável para o spike ser descartável de uma vez;
/// num instalador a sério isto muda para a pasta de dados do utilizador.
/// Onde ficam a identidade e os registos.
///
/// Isto era `dados`, e portanto dependia da pasta de onde a app fosse aberta. Num
/// computador de desenvolvimento não se nota; instalada, nota-se muito: um atalho do menu
/// Iniciar arranca na pasta do executável, e se ela estiver dentro do `Program Files` a
/// app não consegue lá escrever e nem abre. Pior ainda seria abrir e criar uma identidade
/// nova por cada sítio de onde fosse lançada — sem servidor, uma identidade perdida é uma
/// conta perdida.
///
/// A ordem é:
///  1. `BRUMA_DADOS`, para quem quiser mandar (testes, pen, duas contas na mesma máquina);
///  2. uma pasta `dados` já existente ao lado, que é onde vivem as instalações antigas e o
///     ambiente de desenvolvimento — não se abandona o que já lá está;
///  3. a pasta de dados do sistema, que é onde isto devia ter começado.
pub fn raiz() -> PathBuf {
    // BRUMA_DADOS é relido A CADA CHAMADA, de propósito: é o mecanismo dos testes, e cada
    // teste quer a sua pasta. Não passa pelo cache abaixo.
    if let Ok(escolhida) = std::env::var("BRUMA_DADOS") {
        return PathBuf::from(escolhida);
    }
    // EM PRODUÇÃO, resolve-se UMA vez e fica.
    //
    // Antes, `raiz()` era uma função pura recalculada em cada `gravar_indice`, `caminho_do_log`
    // e `registo::caminho`. Dois problemas: o ramo `dados` era `PathBuf::from("dados")`,
    // RELATIVO ao directório de trabalho — se um diálogo nativo ou uma biblioteca mudasse o
    // cwd a meio da sessão, as gravações seguintes iam para outra pasta e o histórico
    // partia-se em dois sem um erro. E a decisão dependia de `dados/identidade.key` existir
    // NAQUELE instante, portanto lançar a app de outro sítio criava uma identidade nova em
    // silêncio. Um `OnceLock` que só cobre este caminho (nunca o de BRUMA_DADOS) resolve os
    // dois sem tocar nos testes.
    static RAIZ: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    RAIZ.get_or_init(resolver_raiz).clone()
}

/// A pasta de dados em produção, resolvida em ABSOLUTO e estável ao cwd.
fn resolver_raiz() -> PathBuf {
    // `dados` ao lado do EXECUTÁVEL, não do directório de trabalho. É o que torna a pasta
    // portátil imune a uma mudança de cwd, que era o bug concreto.
    let ao_lado = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|dir| dir.join("dados")));
    if let Some(d) = &ao_lado {
        if d.join("identidade.key").exists() {
            return d.clone();
        }
    }
    // Senão, a pasta do sistema (%APPDATA%\Bruma ou ~/.local/share/bruma), que já é absoluta.
    pasta_do_sistema()
        .or(ao_lado)
        .unwrap_or_else(|| PathBuf::from("dados"))
}

#[cfg(windows)]
fn pasta_do_sistema() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("Bruma"))
}

#[cfg(not(windows))]
fn pasta_do_sistema() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .map(|d| d.join("bruma"))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServidorGuardado {
    pub id: String,
    /// Chave simétrica do servidor, em hex. Quem a tiver lê tudo.
    pub chave: String,
    /// Peers já conhecidos, para reconectar sem precisar do convite outra vez.
    #[serde(default)]
    pub peers: Vec<String>,
    /// Com quem, se isto for uma conversa privada em vez de um servidor.
    ///
    /// É a única diferença guardada. Por baixo uma conversa é o mesmo que um servidor — o
    /// mesmo log assinado, a mesma cifra, o mesmo caminho de sincronização — e é isso que a
    /// torna barata: nada no que já existe precisa de saber que aquilo é uma conversa.
    #[serde(default)]
    pub com: Option<String>,
    /// Quem me deu o convite. Ver [`Servidor::convidou`].
    #[serde(default)]
    pub convidou: Option<String>,
}

/// Alguém que eu decidi conhecer.
///
/// # O que é uma amizade aqui, e o que não é
///
/// Não há servidor a mediar nada, portanto isto não é um estado partilhado entre duas
/// pessoas: é uma **decisão minha, guardada na minha máquina**. Eu ter-te na lista quer dizer
/// que eu estou disposto a ligar-me a ti — e ligar-me a ti mostra-te o meu IP quando a
/// ligação é directa, que é o caso normal. Por isso é que a lista é minha e não é negociada:
/// ninguém entra nela por me pedir.
///
/// O contrário também vale: alguém pôr-me na lista dele não lhe dá nada. O pedido que ele
/// escreve é uma mensagem, e uma mensagem não abre portas.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Amigo {
    /// A chave pública, que é ao mesmo tempo o endereço de rede.
    pub chave: String,
    /// O nome que EU lhe dei. Não é o que ele diz chamar-se — esse é auto-declarado e o
    /// transporte não o prova. Este é o único que não se pode forjar, porque não viaja.
    pub nome: String,
    pub desde_ms: u64,
    /// Se comparei a chave com ele por outro caminho — voz, papel, olhos nos olhos.
    ///
    /// Numa app sem directório, é isto que substitui «o servidor garante que este é o João».
    /// Enquanto for falso, sabes que falas com quem tem aquela chave; não sabes se aquela
    /// chave é de quem julgas.
    #[serde(default)]
    pub verificado: bool,
}

/// Quem me pode abrir uma conversa.
///
/// Não é o mesmo que «quem me pode encontrar»: ninguém me encontra sem ter a minha chave,
/// porque não há directório. Isto é sobre o que acontece a quem JÁ a tem.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QuemEscreve {
    /// Qualquer pessoa que tenha a minha chave. É o que sempre foi.
    #[default]
    Todos,
    /// Só quem partilha uma sala comigo — o critério que o Discord usa e que aqui é exacto,
    /// porque partilhar uma sala prova-se com a chave dela.
    Salas,
    /// Só quem eu pus na minha lista.
    Amigos,
}

/// Os endereços de um par, e quando foram vistos (#118).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EnderecosDoPar {
    /// `ip:1.2.3.4:5678` ou `relay:https://...`, como o `TransportAddr` os escreve.
    #[serde(default)]
    pub onde: Vec<String>,
    /// Segundos desde a época. Endereços velhos fazem o iroh gastar tempo a tentar caminhos
    /// mortos antes de cair no relay — por isso há uma idade máxima na leitura.
    #[serde(default)]
    pub visto: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Indice {
    #[serde(default)]
    pub servidores: Vec<ServidorGuardado>,
    #[serde(default)]
    pub nome: String,
    /// A chave x25519 de cada pessoa com quem já falámos, para se poder abrir uma conversa
    /// sem esperar que ela esteja online outra vez.
    ///
    /// Não é um segredo: é a metade pública. Guarda-se aqui porque este ficheiro já é
    /// cifrado, e porque quem sabe com quem eu tenho prekeys sabe com quem eu falo.
    #[serde(default)]
    pub prekeys: BTreeMap<String, String>,
    /// Onde cada par foi encontrado da última vez, e quando (#118).
    ///
    /// # Porque é que isto precisa de existir
    ///
    /// O `ligar` faz `connect(EndpointAddr::from(id))` — só o identificador, sem endereço
    /// nenhum. Quem descobre o endereço é o serviço de descoberta do preset do n0, que
    /// resolve por HTTPS e por DNS contra servidores deles. E o que se guardava em disco de
    /// cada par era só a chave.
    ///
    /// Consequência: sem os servidores do n0 — em baixo, bloqueados, ou simplesmente sem
    /// DNS — duas pessoas que já falaram mil vezes **não se encontram**. Numa app cuja
    /// promessa é não depender de servidor nenhum, essa é a dependência escondida.
    ///
    /// Guarda-se como texto e não como o tipo do iroh de propósito: este ficheiro tem de
    /// sobreviver a actualizações da biblioteca.
    #[serde(default)]
    pub enderecos: BTreeMap<String, EnderecosDoPar>,
    #[serde(default)]
    pub amigos: Vec<Amigo>,
    /// Chaves de quem eu recuso. **O bloqueio é local**: eu deixo de aceitar o que ele
    /// manda, não o impeço de tentar. Não há servidor no meio para o impedir por mim.
    ///
    /// Em compensação, ele não distingue estar bloqueado de eu estar offline — o que é mais
    /// do que o Discord dá.
    #[serde(default)]
    pub bloqueados: Vec<String>,
    /// Até quando é que cada canal já foi lido, em ms. A chave é `"{servidor}/{canal}"`.
    ///
    /// Vive no índice, que já é cifrado, e por uma razão que não é só comodidade: saber que
    /// canais eu leio e quando é saber a minha rotina. Num ficheiro à parte e em claro, isso
    /// ficava legível a quem abrisse a pasta.
    ///
    /// É por TEMPO e não por contagem: uma contagem obriga a saber quantas mensagens havia
    /// quando li, e isso muda quando chega histórico antigo de outro par — de repente eu
    /// teria «não lidas» de coisas que já tinha lido. O tempo da última mensagem que eu vi
    /// não muda por chegar histórico.
    #[serde(default)]
    pub lido: BTreeMap<String, i64>,
    #[serde(default)]
    pub quem_escreve: QuemEscreve,
    /// Tudo o que uma versão MAIS RECENTE tenha escrito aqui e que esta não conhece.
    ///
    /// O serde descarta campos desconhecidos por omissão. Sem isto: instala-se a v0.19 que
    /// acrescenta um campo ao índice; a actualização falha ou reinstala-se uma release
    /// anterior; a v0.18 lê o índice, ignora o campo novo, e a primeira gravação apaga-o
    /// PERMANENTEMENTE. Com a rotação de chave de sala e a forward secrecy no horizonte — as
    /// duas guardam estado novo — isto deixa de ser teórico e passa a ser perda de chaves.
    ///
    /// Com o `flatten`, os campos desconhecidos são lidos e reescritos intactos por qualquer
    /// versão. Engole também campos escritos por engano; é o preço, e é muito menor.
    #[serde(flatten, default)]
    pub resto: BTreeMap<String, serde_json::Value>,
}

pub struct Servidor {
    pub id: String,
    pub chave: [u8; 32],
    pub log: blog::Log,
    /// Quem PROVOU pertencer aqui. Ver [`Servidor::autores_provados`].
    pub peers: Vec<String>,
    /// Quem me deu o convite. **Não é o mesmo que pertencer.**
    ///
    /// Um convite é JSON em base32 sem assinatura nenhuma: quem o escreve põe lá o nome que
    /// quiser no campo `anfitriao`. Estava a ir directo para os `peers` — a lista que decide
    /// quem me pode pôr som nas colunas e forjar presença. Bastava alguém dar-me um convite
    /// com a chave de um terceiro lá dentro para esse terceiro ganhar direitos de sala sobre
    /// mim, sem nunca ter provado nada.
    ///
    /// Continua a servir para o que realmente precisa: discar-lhe e trocar o histórico
    /// **daquela sala**. Assim que ele escrever uma entrada que DECIFRA, entra nos `peers`
    /// pela porta da frente.
    pub convidou: Option<String>,
    pub com: Option<String>,
    /// Cache de `autores_provados`, PREGUIÇOSA (#99): nasce vazia e enche-se na primeira
    /// vez que alguém pergunta — que é quando alguém se liga (`aprender_dos_logs`, o
    /// porteiro do `aplicar`) — e não no arranque, onde era uma passagem de decifragem
    /// completa POR servidor antes de a janela pintar.
    ///
    /// Enquanto está por inicializar, `escrever`/`merge_contado` não lhe tocam: a
    /// inicialização lê o log, que já inclui essas entradas. Depois de inicializada,
    /// mantêm-na eles. NUNCA `get().unwrap()` — é `autores_provados()` que a garante.
    ///
    /// Sem cache nenhuma, o `aprender_dos_logs` decifrava TODOS os logs — com o lock global
    /// preso — a cada ligação de qualquer estranho. Era um caminho de negação de serviço
    /// aberto por mim ao fechar outro.
    provados: std::cell::OnceCell<std::collections::BTreeSet<String>>,
}

/// Quanto é que o relógio de outra pessoa pode estar adiantado antes de eu deixar de
/// acreditar nele — só para efeitos de «por ler».
///
/// O `ts_ms` de uma entrada é escolhido por quem a escreve: o `merge` verifica a assinatura,
/// que é feita com a chave do próprio autor. Não há aqui nada que impeça o ano 9999.
///
/// **Aparar para «agora» não chega, e foi o meu primeiro instinto.** Com um tecto móvel, a
/// entrada forjada vale sempre «agora» — portanto está sempre à frente da última marca de
/// leitura, e o vermelho volta a acender a cada redesenho. Trocava um vermelho preso por um
/// vermelho intermitente.
///
/// O que é estável é EXCLUIR: a entrada não conta e não mexe na marca, hoje e daqui a um ano.
/// Continua a aparecer no canal — não se esconde nada, só se deixa de a usar para decidir o
/// que está por ler.
///
/// Um dia é muito mais do que qualquer desvio real entre duas máquinas, e o custo de errar
/// para este lado é pequeno: uma mensagem de alguém com o relógio um dia adiantado não
/// acende a bolha. Está dito no painel.
const DESVIO_TOLERADO_MS: i64 = 24 * 60 * 60 * 1000;

/// Um `App` e um `Servidor` de teste, sem tocar no disco.
///
/// Os testes que já existem neste ficheiro usam o `App::arrancar()` com o `BRUMA_DADOS`
/// apontado a uma pasta temporária — e por isso precisam do `trava_dados()`, porque a variável
/// é global ao processo e o `cargo test` corre em paralelo. Isso é necessário para testar o
/// ARRANQUE; é peso morto para testar uma decisão que só lê o mapa de servidores.
///
/// Estes constroem o estado directamente: sem env vars, sem pasta, sem serialização — logo sem
/// mutex de teste e sem ordem entre testes. O log aponta para um caminho que nunca é escrito
/// (nenhum destes testes anexa nada), e é o `Log::load` de um ficheiro inexistente que dá um
/// log vazio, que é exactamente o que se quer.
#[cfg(test)]
impl App {
    pub(crate) fn para_teste() -> std::sync::Arc<Self> {
        let semente = [7u8; 32];
        std::sync::Arc::new(App {
            ident: crypto::Identity::from_seed(&semente),
            semente,
            nome: Mutex::new("eu".into()),
            servidores: Mutex::new(BTreeMap::new()),
            prekeys: Mutex::new(BTreeMap::new()),
            enderecos: Mutex::new(BTreeMap::new()),
            amigos: Mutex::new(Vec::new()),
            bloqueados: Mutex::new(Vec::new()),
            quem_escreve: Mutex::new(QuemEscreve::default()),
            lido: Mutex::new(BTreeMap::new()),
            escrita_do_indice: Mutex::new(()),
            nao_abriram: Mutex::new(Vec::new()),
            resto_do_indice: Mutex::new(BTreeMap::new()),
            congelada: std::sync::atomic::AtomicBool::new(false),
        })
    }
}

/// Medição: passagens de decifragem pelo log (uma por `aplicaveis`). Abrir um canal devia
/// custar UMA.
pub static DECIFRAGENS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
thread_local! {
    /// O mesmo contador, so desta thread: e o que um teste le, porque os testes correm em
    /// paralelo e o estatico soma o que as outras threads fazem.
    pub static DECIFRAGENS_NA_THREAD: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
/// Medição: quantas vezes a cabeça em cache do log foi apanhada diferente da recalculada.
pub static CABECA_DESSINCRONIZADA: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
impl Servidor {
    pub(crate) fn para_teste(id: &str) -> Self {
        let caminho =
            std::env::temp_dir().join(format!("bruma-porteiro-{}-{id}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&caminho);
        Servidor::novo(
            id.to_string(),
            [0u8; 32],
            blog::Log::load(&caminho).expect("um log vazio"),
            Vec::new(),
            None,
            None,
        )
    }
}

/// Se as entradas novas vão presas ao autor. **Ainda não — e a razão é o outro lado.**
///
/// # A travessia, e porque é que ela tem dois passos
///
/// Prender a cifra ao autor (#197) muda o formato: uma entrada selada assim **não abre** numa
/// versão que não conheça o truque. E a versão anterior não conhece — o `merge_verificado`
/// dela filtra por um `crypto::open` sem dados associados, e o que não abre é deitado fora
/// sem uma linha em lado nenhum.
///
/// Ou seja: se esta versão começasse já a escrever assim, cada mensagem que eu escrevesse
/// desaparecia no caminho para quem ainda não tivesse actualizado. Não «chegava com erro» —
/// desaparecia. Numa app de duas pessoas em dois continentes, isso é a pior avaria possível,
/// e é exactamente a que este projecto passou versões a fechar.
///
/// O procedimento está escrito no plano de transição do `canonical()` e vale aqui tal e qual:
///
/// 1. **Primeiro publica-se uma versão que LÊ as duas formas e escreve a antiga.** É esta.
///    A defesa contra o `ciphertext` copiado já está de pé para tudo o que chegue selado —
///    o que falta é só passar a selar.
/// 2. **Confirma-se que as duas máquinas estão nela.** A app diz a versão do outro lado
///    desde a v0.18.0, e foi para isto que essa funcionalidade serve.
/// 3. **Só então se põe isto a `true` e se publica.** A partir daí, quem não actualizou
///    deixa de ler o que é novo — mas com a versão à frente dele no ecrã a dizer porquê, em
///    vez de mensagens a evaporarem-se.
///
/// Deixar isto a `false` para sempre seria deixar o buraco aberto para sempre. É uma linha,
/// e o dia de a mudar é o dia em que as duas casas estiverem na mesma versão.
const ESCREVER_PRESO_AO_AUTOR: bool = false;

/// Decifra a carga de uma entrada, aceitando as duas formas — e a leitura dupla é SEGURA.
///
/// # Porque é que a alternativa não abre uma porta (#197)
///
/// A tentação, ao ver um `else`, é dizer: «então o atacante usa a forma antiga e passa na
/// mesma». Não passa, e a razão está no AEAD: a etiqueta de autenticação COBRE o `aad`. Um
/// `ciphertext` selado com o autor lá dentro **não abre sem ele** — tentar abri-lo à moda
/// antiga falha tal como tentar abri-lo com o autor errado.
///
/// Portanto os três casos são:
///
/// | a entrada | com AAD | sem AAD | resultado |
/// |---|---|---|---|
/// | selada com o autor certo | abre | — | aceite |
/// | selada, mas copiada sob outro autor | falha | falha | **recusada** |
/// | selada sem autor (antiga, ou desta versão) | falha | abre | aceite |
///
/// # E o que isto NÃO protege hoje — que é preciso dizer com todas as letras
///
/// Enquanto o [`ESCREVER_PRESO_AO_AUTOR`] estiver a `false`, **esta versão não sela nada com
/// o autor**. Logo nenhuma entrada que ela produza cai na primeira linha da tabela: todas
/// caem na terceira, e uma cópia delas sob outro nome continua a abrir pelo `else` e continua
/// a tornar quem a mandou um membro provado.
///
/// Ou seja: o ataque do `ciphertext` copiado **ainda não está fechado em produção**. O que
/// está feito é a metade que não parte nada — o lado da leitura, que reconhece e protege o
/// que vier selado. A outra metade é uma linha, e o dia dela está escrito no
/// `ESCREVER_PRESO_AO_AUTOR`: quando as duas casas estiverem na mesma versão.
///
/// Escrever isto aqui em vez de deixar a tabela sozinha não é pedantismo. Uma tabela que
/// descreve a defesa completa, ao lado de código que só entrega metade, é a forma mais fácil
/// de alguém — eu, daqui a três meses — ler «recusada» e dar o problema por resolvido.
fn decifrar_carga(chave: &[u8; 32], e: &blog::Entry) -> Option<Vec<u8>> {
    let nonce = hex24(&e.nonce).ok()?;
    let ct = HEXLOWER.decode(e.ciphertext.as_bytes()).ok()?;
    let autor = hex32(&e.author).ok()?;
    crypto::open_com(chave, &nonce, &ct, &autor)
        .or_else(|_| crypto::open(chave, &nonce, &ct))
        .ok()
}

impl Servidor {
    /// O único caminho para construir um `Servidor`.
    ///
    /// A cache `provados` é privada de propósito: se fosse pública, alguém a preencheria à
    /// mão e o «provado» deixava de querer dizer alguma coisa. Aqui ela é sempre derivada do
    /// log — e só quando for precisa (#99): construir um servidor não decifra nada.
    pub fn novo(
        id: String,
        chave: [u8; 32],
        log: blog::Log,
        peers: Vec<String>,
        convidou: Option<String>,
        com: Option<String>,
    ) -> Self {
        Servidor {
            id,
            chave,
            log,
            peers,
            convidou,
            com,
            provados: Default::default(),
        }
    }

    /// Decifra o que conseguir e devolve as entradas prontas a aplicar, pela ordem do log.
    ///
    /// O que não decifrar é **ignorado em silêncio, não rejeitado**: numa app onde a chave
    /// pode rodar, ter entradas que já não se conseguem ler é normal, não é corrupção.
    pub fn aplicaveis(&self) -> (Vec<Aplicavel>, Vec<String>) {
        DECIFRAGENS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        DECIFRAGENS_NA_THREAD.with(|c| c.set(c.get() + 1));
        let mut saida = Vec::new();
        let mut ids = Vec::new();
        // Por referência e com o instante já calculado (#154, #198): antes clonava-se o log
        // inteiro e recalculava-se o hash de cada entrada — a partir da chave do mapa onde
        // ela já estava — a cada redesenho.
        for (instante, id, e) in self.log.ordered_ref() {
            let Some(claro) = decifrar_carga(&self.chave, e) else {
                continue;
            };
            // LEITURA EM DOIS PASSOS (#8, #20).
            //
            // Uma carga que esta versão não reconhece caía aqui num `continue`: desaparecia
            // da vista, sem erro, sem sinal — e ficava guardada no disco na mesma. A app
            // guardava aquilo que se recusava a mostrar, e não o dizia. Agora, se a `Carga`
            // não se lê, tenta-se ler o mínimo (a etiqueta e o canal) para se poder pôr um
            // marcador no sítio certo. Só se deita fora o que nem sequer é uma carga.
            let carga = match serde_json::from_slice::<Carga>(&claro) {
                Ok(c) => c,
                Err(_) => match serde_json::from_slice::<modelo::CargaCrua>(&claro) {
                    Ok(crua) => Carga::Desconhecida {
                        etiqueta: crua.t,
                        canal: crua.canal,
                    },
                    Err(_) => continue,
                },
            };
            saida.push(Aplicavel {
                autor: e.author.clone(),
                ts_ms: e.ts_ms,
                instante,
                carga,
            });
            ids.push(id.to_string());
        }
        (saida, ids)
    }

    /// Quem PROVOU pertencer aqui.
    ///
    /// Não é «quem assinou uma entrada»: o `merge` verifica a assinatura e mais nada, portanto
    /// qualquer pessoa consegue anexar lixo assinado por ela própria e passar por autor. A
    /// prova a sério é uma entrada **que decifra** — isso exige a chave simétrica desta sala,
    /// que só se recebe no convite.
    ///
    /// É este o conjunto que o porteiro da rede usa. Enquanto era «quem se ligou e disse o id»,
    /// bastava uma mensagem vazia para entrar.
    pub fn autores_provados(&self) -> &std::collections::BTreeSet<String> {
        self.provados
            .get_or_init(|| self.aplicaveis().0.into_iter().map(|a| a.autor).collect())
    }

    /// Quantas mensagens por canal ficaram por ler, e a hora da mais recente.
    ///
    /// As minhas não contam — ver a minha própria mensagem como «não lida» seria a app a
    /// avisar-me de que eu falei.
    ///
    /// Uma passagem só pelo log, e não uma por canal: com dez canais, uma por canal seria
    /// decifrar tudo dez vezes.
    ///
    /// `contaveis` é a lista de canais que a pessoa CONSEGUE abrir. Sem ela, contava-se tudo
    /// o que aparecesse no log — e havia três maneiras de ficar com uma bolha vermelha que
    /// nunca mais se apaga:
    ///
    /// - o **chat da sala** escreve mensagens reais dentro do canal de VOZ, e um canal de voz
    ///   não se abre como texto: a marca de leitura nunca avança;
    /// - um canal **apagado** (e qualquer membro pode apagar canais) leva com ele a única
    ///   forma de o abrir, mas as mensagens ficam no log;
    /// - um canal **inventado** por quem escreve — o id da carga não é verificado contra
    ///   nada — dava a qualquer membro a capacidade de me pôr um vermelho permanente na
    ///   barra, e de mandar o nome que quisesse para o aviso do sistema.
    ///
    /// Contar só o que se pode abrir fecha os três de uma vez, na origem.
    /// Só os testes lhe chamam — o caminho da app usa o  com as
    /// entradas que já decifrou.  e não : a primeira diz
    /// o que isto é, a segunda só cala o compilador.
    #[cfg(test)]
    pub fn nao_lidos(
        &self,
        eu: &str,
        lido: &BTreeMap<String, i64>,
        contaveis: &std::collections::BTreeSet<String>,
    ) -> BTreeMap<String, (usize, i64)> {
        // Uma casca sobre a implementação única. O caminho quente (`estado`) já traz as
        // entradas decifradas e chama o outro directamente; isto existe para quem só quer a
        // contagem — hoje, os testes. Duas cópias da mesma regra são uma regra que um dia
        // deixa de ser a mesma, e a regra aqui é «o que conta como por ler».
        let (aps, _) = self.aplicaveis();
        self.por_ler_das_entradas(&aps, eu, lido, contaveis)
    }

    /// A hora da mensagem mais recente de um canal que NÃO é minha.
    ///
    /// O `eu` não é um detalhe de simetria com o `nao_lidos`: sem ele, escrever uma mensagem
    /// fazia a marca de leitura avançar, e cada avanço reescreve o índice inteiro, cifrado,
    /// no disco. Uma conversa activa passava a dar uma reescrita completa por cada coisa que
    /// eu digo — que o comando de enviar nunca fazia — e cada uma dessas escritas é uma
    /// janela onde uma falha de energia custa as chaves todas.
    ///
    /// E não muda nada no que se vê: as minhas nunca contaram como por ler.
    ///
    /// Um `ts_ms` demasiado à frente é IGNORADO — ver [`DESVIO_TOLERADO_MS`]. Sem isso, uma
    /// única mensagem com o ano 9999 marcava o canal como lido para sempre: tudo o que
    /// viesse depois nascia já «lido», em silêncio.
    pub fn ultima_mensagem(&self, canal: &str, eu: &str) -> i64 {
        Self::ultima_das_entradas(&self.aplicaveis().0, canal, eu)
    }

    /// A marca até onde um canal fica lido, a partir de entradas JÁ decifradas — para quem
    /// abre um canal fazer UMA passagem (#90).
    ///
    /// O VALOR é o instante efectivo (#198), o mesmo eixo do por-ler; o FILTRO do veneno
    /// fica no carimbo cru, de propósito: o instante propaga-se (`max(ts, pai + 1)`) e um
    /// ano 9999 empurraria todos os descendentes para lá do limite — filtrar pelo instante
    /// calava a contagem de tudo o que viesse depois do veneno, para sempre. Com o filtro no
    /// cru o veneno não conta, os descendentes contam com instantes enormes, a marca avança
    /// até eles, e os seguintes (pai + 1) continuam a contar.
    pub fn ultima_das_entradas(aps: &[Aplicavel], canal: &str, eu: &str) -> i64 {
        let limite = agora_ms() as i64 + DESVIO_TOLERADO_MS;
        aps.iter()
            .filter_map(|a| match &a.carga {
                Carga::Mensagem { canal: c, .. } if c == canal && a.autor != eu => {
                    let ts = a.ts_ms.min(i64::MAX as u64) as i64;
                    (ts <= limite).then_some(a.instante.min(i64::MAX as u64) as i64)
                }
                _ => None,
            })
            .max()
            .unwrap_or(0)
    }

    /// Abrir um canal numa passagem só (#90): as mensagens e a marca até onde ele fica lido.
    /// Antes eram duas passagens pelo log (`mensagens` + `marcar_lido`) por cada abertura.
    pub fn abrir_canal(&self, canal: &str, eu: &str) -> (Vec<MensagemVista>, i64) {
        let (aps, ids) = self.aplicaveis();
        let estado = modelo::reconstruir(&aps);
        let mensagens = modelo::mensagens_do_canal(&aps, &ids, canal, &estado);
        let ultima = Self::ultima_das_entradas(&aps, canal, eu);
        (mensagens, ultima)
    }

    /// O estado e o que falta ler, de UMA passagem pelo log.
    ///
    /// Chamar `estado()` e depois `nao_lidos()` decifra o log inteiro duas vezes, e o comando
    /// que os usa segura o lock de todos os servidores durante as duas. Foi o que eu fiz ao
    /// acrescentar o por-ler: dobrei o custo do caminho mais quente da app — corre a cada
    /// redesenho e a cada mensagem que chega — sem reparar.
    ///
    /// O custo continua linear no histórico, que é um problema conhecido e maior do que este.
    /// O que aqui se corrige é ter passado a ser o dobro.
    pub fn estado_e_entradas(&self) -> (EstadoDoServidor, Vec<Aplicavel>) {
        let (aps, _) = self.aplicaveis();
        // A passagem que já se faz semeia a cache dos provados de graça (#99).
        let _ = self
            .provados
            .get_or_init(|| aps.iter().map(|a| a.autor.clone()).collect());
        let estado = modelo::reconstruir(&aps);
        (estado, aps)
    }

    /// A contagem, a partir de entradas JÁ decifradas.
    pub fn por_ler_das_entradas(
        &self,
        aps: &[Aplicavel],
        eu: &str,
        lido: &BTreeMap<String, i64>,
        contaveis: &std::collections::BTreeSet<String>,
    ) -> BTreeMap<String, (usize, i64)> {
        let limite = agora_ms() as i64 + DESVIO_TOLERADO_MS;
        let mut fora: BTreeMap<String, (usize, i64)> = BTreeMap::new();
        for a in aps {
            let Carga::Mensagem { canal, .. } = &a.carga else {
                continue;
            };
            if a.autor == eu || !contaveis.contains(canal) {
                continue;
            }
            let ts = a.ts_ms.min(i64::MAX as u64) as i64;
            if ts > limite {
                continue;
            }
            let ate = lido
                .get(&App::chave_de_leitura(&self.id, canal))
                .copied()
                .unwrap_or(0);
            // A comparação é no INSTANTE (#198): uma mensagem escrita depois de um relógio
            // corrigido tem um carimbo cru menor do que a marca e um instante maior — com o
            // carimbo cru ficava escondida do por-ler, para sempre.
            let instante = a.instante.min(i64::MAX as u64) as i64;
            if instante > ate {
                let e = fora.entry(canal.clone()).or_insert((0, 0));
                e.0 += 1;
                e.1 = e.1.max(instante);
            }
        }
        fora
    }

    pub fn estado(&self) -> EstadoDoServidor {
        let (aps, _) = self.aplicaveis();
        modelo::reconstruir(&aps)
    }

    pub fn mensagens(&self, canal: &str) -> Vec<MensagemVista> {
        let (aps, ids) = self.aplicaveis();
        let estado = modelo::reconstruir(&aps);
        modelo::mensagens_do_canal(&aps, &ids, canal, &estado)
    }

    /// Cifra uma carga e junta-a ao log. Devolve a entrada para ser difundida aos peers.
    pub fn escrever(&mut self, signing: &SigningKey, carga: &Carga) -> Result<blog::Entry> {
        let claro = serde_json::to_vec(carga)?;
        // AINDA SEM O AUTOR — e a data de mudar isto é uma decisão, não um esquecimento.
        //
        // Ver [`ESCREVER_PRESO_AO_AUTOR`]. Esta versão LÊ as duas formas e ESCREVE a antiga,
        // de propósito: escrever a nova hoje fazia a outra máquina — que ainda corre a
        // versão anterior — deitar fora cada mensagem minha, em silêncio.
        let (nonce, ct) = if ESCREVER_PRESO_AO_AUTOR {
            crypto::seal_com(
                &self.chave,
                &claro,
                signing.verifying_key().as_bytes().as_slice(),
            )?
        } else {
            crypto::seal(&self.chave, &claro)?
        };
        let e = self.log.append_local(signing, nonce, ct, agora_ms())?;
        // A cabeça em cache confere-se contra a recalculada em cada escrita — só em debug,
        // que é onde os guiões correm, e sem bandeira: é verificação, não um andaime.
        #[cfg(debug_assertions)]
        {
            let calc = self.log.cabeca_recalculada().map(|(_, h)| h);
            if calc.as_deref() != Some(self.log.head().as_str()) {
                eprintln!("[log] CABEÇA EM CACHE ≠ RECALCULADA em {}", self.id);
                CABECA_DESSINCRONIZADA.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
        // Só se a cache já existe: por inicializar, será lida do log, que já tem isto.
        if let Some(p) = self.provados.get_mut() {
            p.insert(e.author.clone());
        }
        Ok(e)
    }

    /// Junta entradas ao log, guardando **só as que decifram**.
    ///
    /// O `blog::Log::merge` verifica a assinatura e mais nada — e a assinatura é feita com a
    /// chave de quem escreve, portanto qualquer pessoa assina o que quiser. O resultado era
    /// que um estranho anexava lixo ao meu ficheiro, para sempre, sem limite de tamanho, e
    /// passava a constar como autor de uma sala onde nunca entrou.
    ///
    /// Numa sala, tudo o que é legítimo está cifrado com a chave dela. Uma entrada que não
    /// decifra não é uma entrada daquela sala — é ruído que alguém me mandou guardar.
    ///
    /// QUANDO A ROTAÇÃO DE CHAVE EXISTIR, isto tem de passar a «decifra com alguma chave que
    /// eu tenha, actual ou reformada». Hoje não há rotação — e é por isso que se pode ser
    /// tão estrito. Fica escrito para não se descobrir tarde.
    /// E a dizer quantas ficaram de fora (#85).
    ///
    /// Uma entrada que não decifra com a chave desta sala é, por definição, de quem NÃO tem a
    /// chave. Uma legítima decifra sempre — quem pertence tem-na. Portanto a contagem de
    /// recusas é o sinal que distingue «alguém a sincronizar» de «alguém a atirar lixo», e é
    /// esse número que o porteiro da entrada gasta.
    ///
    /// Substituiu o `merge_verificado`, que devolvia só as novas: um caminho que não conta o
    /// lixo é um caminho por onde o porteiro não vê nada, e dois caminhos para o mesmo merge
    /// seriam a receita para alguém escolher o cego sem dar por isso.
    pub fn merge_contado(&mut self, entradas: Vec<blog::Entry>) -> Result<(usize, usize)> {
        let total = entradas.len();
        let boas: Vec<blog::Entry> = entradas
            .into_iter()
            .filter(|e| {
                // A ASSINATURA PRIMEIRO, E NO MESMO FILTRO.
                //
                // Isto verificava só a decifragem aqui, e deixava a assinatura para o
                // `log.merge` — que corre DEPOIS do `provados.insert`. O `author` de uma
                // entrada com assinatura de lixo era adoptado como provado, e o `merge`
                // rejeitava a entrada a seguir: o autor ficava, a entrada não.
                //
                // E `provados` é a base de tudo. Ele alimenta o `autores_provados()`, que o
                // `aplicar` e o `aprender_dos_logs` usam para empurrar chaves para
                // `srv.peers` — a lista que decide quem me põe som nas colunas. Como o
                // `peers` é gravado no índice, ficava para sempre.
                //
                // Qualquer membro da sala consegue fazer uma entrada que decifra (tem a
                // chave). Bastava-lhe pôr no `author` a chave de um terceiro e assinar com
                // lixo, e esse terceiro ganhava direitos de sala sem nunca lá ter entrado.
                //
                // Construí a cadeia inteira sobre «a prova é uma entrada que DECIFRA» e
                // depois enchi o conjunto antes de a prova estar completa.
                if e.verify().is_err() {
                    return false;
                }
                decifrar_carga(&self.chave, e).is_some()
            })
            .collect();
        if let Some(p) = self.provados.get_mut() {
            for e in &boas {
                p.insert(e.author.clone());
            }
        }
        let recusadas = total - boas.len();
        Ok((self.log.merge(boas)?, recusadas))
    }
}

pub struct App {
    pub ident: crypto::Identity,
    pub semente: [u8; 32],
    pub nome: Mutex<String>,
    pub servidores: Mutex<BTreeMap<String, Servidor>>,
    pub prekeys: Mutex<BTreeMap<String, String>>,
    /// Onde cada par foi encontrado da ultima vez (#118). Ver `Indice::enderecos`.
    pub enderecos: Mutex<BTreeMap<String, EnderecosDoPar>>,
    pub amigos: Mutex<Vec<Amigo>>,
    pub bloqueados: Mutex<Vec<String>>,
    pub quem_escreve: Mutex<QuemEscreve>,
    /// Ver [`Indice::lido`].
    pub lido: Mutex<BTreeMap<String, i64>>,
    /// Serializa `gravar_indice` — duas escritas ao mesmo tempo destruíam o índice. Não guarda
    /// nada; é só um portão. É privado de propósito: se alguém o tomasse antes de chamar
    /// `gravar_indice`, prendia.
    escrita_do_indice: Mutex<()>,
    /// As salas que não abriram no arranque (chave estragada, log ilegível). Guardam-se tal e
    /// qual — sobretudo a chave — e o `gravar_indice` reescreve-as, para a primeira gravação
    /// não apagar do índice a única forma de decifrar o ficheiro que ficou no disco.
    nao_abriram: Mutex<Vec<ServidorGuardado>>,
    /// O que uma versão MAIS RECENTE escreveu no índice e esta não conhece. Guarda-se do
    /// arranque até à gravação para ser devolvido intacto — ver [`Indice::resto`].
    resto_do_indice: Mutex<BTreeMap<String, serde_json::Value>>,
    /// Depois de uma restauração de identidade, a semente em `self.semente` é a ANTIGA e a do
    /// disco é a nova. Qualquer `gravar_indice` cifraria com a antiga e deixaria um índice que
    /// no arranque seguinte não abre. Enquanto isto estiver `true`, o `gravar_indice` recusa.
    congelada: std::sync::atomic::AtomicBool,
}

impl App {
    pub fn arrancar() -> Result<Self> {
        let raiz = raiz();
        std::fs::create_dir_all(raiz.join("servidores"))?;
        let semente = semente_ou_cria(&raiz.join("identidade.key"))?;
        let ident = crypto::Identity::from_seed(&semente);

        let (indice, em_claro) = ler_indice(&raiz, &semente)?;
        let mut servidores = BTreeMap::new();
        // As salas que NAO abriram neste arranque, guardadas tal e qual. Ver o campo
        // `nao_abriram` da App e o `gravar_indice`. Sem isto, a primeira gravação apagava-as
        // do índice — e com elas a chave, que é a única forma de decifrar o ficheiro que
        // ficou no disco.
        let mut nao_abriram: Vec<ServidorGuardado> = Vec::new();
        for s in &indice.servidores {
            // UM SERVIDOR MAU NÃO LEVA A APP COM ELE.
            //
            // Antes, qualquer um destes erros subia até ao `.expect()` do `main` e a app
            // morria — e como o binário de release não tem consola, morria em SILÊNCIO: a
            // janela abria, piscava e desaparecia. Um byte trocado num ficheiro de
            // servidor transformava o Bruma numa app que não abre e não diz porquê, sem
            // forma de recuperar sem ir apagar ficheiros à mão.
            //
            // Agora o servidor estragado fica de fora, os outros abrem, e a razão fica
            // escrita no registo. Perder uma conversa é mau; perder a app inteira, e a
            // possibilidade de perceber porquê, é muito pior.
            let chave = match hex32(&s.chave) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!(
                        "[dados] o servidor {} tem a chave estragada ({e}); fica de fora,                          mas a chave continua guardada no índice",
                        s.id
                    );
                    nao_abriram.push(s.clone());
                    continue;
                }
            };
            let caminho = raiz.join("servidores").join(format!("{}.json", s.id));
            let log = match blog::Log::load(caminho.clone()) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!(
                        "[dados] não consegui ler o servidor {} ({e}); fica de fora",
                        s.id
                    );
                    // O ficheiro mau é posto de lado, e não apagado: pode ser a única cópia
                    // de uma conversa, e quem souber ler JSON ainda a tira de lá. E a entrada
                    // do índice — sobretudo a CHAVE — guarda-se tal e qual, senão a próxima
                    // gravação apagava a única forma de algum dia decifrar esse ficheiro.
                    pos_de_lado(&caminho);
                    nao_abriram.push(s.clone());
                    continue;
                }
            };
            let srv = Servidor {
                id: s.id.clone(),
                chave,
                log,
                peers: s.peers.clone(),
                convidou: s.convidou.clone(),
                com: s.com.clone(),
                provados: Default::default(),
            };
            // Nenhuma passagem de decifragem aqui (#99): a cache dos provados enche-se
            // quando alguém se ligar, ou de graça na primeira `estado_e_entradas`.
            servidores.insert(s.id.clone(), srv);
        }

        // Um amigo posto pela linha de comandos, para se poder medir o que a amizade serve:
        // falar com alguem com quem NAO se partilha servidor nenhum. Sem isto, o unico teste
        // possivel seria entre duas pessoas do mesmo servidor -- onde a amizade nao muda nada
        // e o teste passaria sem provar o que se afirma.
        // Só em debug, e com `#[cfg]` e não `cfg!()`.
        //
        // Com `cfg!(debug_assertions)` o comportamento fica certo, mas o texto
        // "BRUMA_AMIGO" continua no binário e a chamada ao ambiente continua a ser feita —
        // fica a depender do optimizador não a ter deixado lá. Medi: com `cfg!()` uma das
        // duas bandeiras desapareceu do exe de release e a outra ficou. «O optimizador
        // provavelmente tira» não é uma garantia; `#[cfg]` é.
        #[cfg(debug_assertions)]
        let amigo_de_teste = std::env::var("BRUMA_AMIGO").ok();
        #[cfg(not(debug_assertions))]
        let amigo_de_teste: Option<String> = None;

        let app = App {
            ident,
            semente,
            nome: Mutex::new(indice.nome),
            servidores: Mutex::new(servidores),
            prekeys: Mutex::new(indice.prekeys),
            enderecos: Mutex::new(indice.enderecos),
            amigos: Mutex::new(indice.amigos),
            bloqueados: Mutex::new(indice.bloqueados),
            quem_escreve: Mutex::new(indice.quem_escreve),
            lido: Mutex::new(indice.lido),
            escrita_do_indice: Mutex::new(()),
            nao_abriram: Mutex::new(nao_abriram),
            resto_do_indice: Mutex::new(indice.resto.clone()),
            congelada: std::sync::atomic::AtomicBool::new(false),
        };

        #[cfg(debug_assertions)]
        if let Ok(chave) = std::env::var("BRUMA_BLOQUEIA") {
            match app.bloquear(&chave, true) {
                Ok(()) => eprintln!(
                    "[bloqueio] {} recusado pela linha de comandos",
                    &chave[..8.min(chave.len())]
                ),
                Err(e) => eprintln!("[bloqueio] não consegui bloquear: {e}"),
            }
        }

        if let Some(chave) = amigo_de_teste {
            match app.adicionar_amigo(&chave, "amigo de teste") {
                Ok(()) => eprintln!(
                    "[amigos] {} entrou na lista pela linha de comandos",
                    &chave[..8.min(chave.len())]
                ),
                Err(e) => eprintln!("[amigos] não consegui pôr o amigo de teste: {e}"),
            }
        }

        // A CONVERSÃO ACONTECE AGORA, e não "na próxima gravação".
        //
        // A primeira versão disto deixava o ficheiro em claro até alguém criar um canal ou
        // mudar o nome — e quem não fizesse nada ficava com as chaves de todos os servidores
        // legíveis, para sempre, depois de ter actualizado precisamente para as esconder.
        // Uma correcção que só se aplica a quem mexe na app não é uma correcção.
        //
        // É seguro fazê-lo aqui: tudo já carregou, e se a gravação falhar o ficheiro antigo
        // continua onde estava. Não se perde nada por tentar.
        if em_claro {
            match app.gravar_indice() {
                Ok(()) => eprintln!("[dados] o índice estava em texto simples; ficou cifrado"),
                Err(e) => eprintln!("[dados] não consegui cifrar o índice ({e}); fica como estava"),
            }
        }
        Ok(app)
    }

    /// A semente crua. Só o comando das palavras lhe toca — e por isso é que ela não é
    /// pública sem mais: quem a tem, é a pessoa.
    pub fn semente_bruta(&self) -> &[u8; 32] {
        &self.semente
    }

    pub fn minha_chave(&self) -> String {
        HEXLOWER.encode(self.ident.verifying().as_bytes())
    }

    /// Guarda a chave de conversa de alguém, aprendida quando nos ligámos.
    ///
    /// Só a metade pública, e verificada: o `verify_prekey` prova que aquela chave x25519
    /// foi anunciada por quem diz ser dono dela. Sem essa verificação, qualquer um anunciava
    /// a prekey de outro e ficava a ler as conversas dele.
    pub fn guardar_prekey(&self, peer: &str, x_pub: &str, sig: &str) -> Result<()> {
        let dono = ed25519_dalek::VerifyingKey::from_bytes(&hex32(peer)?)
            .map_err(|_| anyhow!("chave de identidade inválida"))?;
        let xb = hex32(x_pub)?;
        let sb: [u8; 64] = HEXLOWER
            .decode(sig.as_bytes())
            .ok()
            .and_then(|v| v.try_into().ok())
            .ok_or_else(|| anyhow!("assinatura da prekey mal formada"))?;
        crypto::verify_prekey(&dono, &xb, &sb)?;

        let mut m = self.prekeys.lock().unwrap();
        match m.get(peer).map(|v| v.as_str()) {
            Some(ja) if ja == x_pub => return Ok(()), // já a tínhamos, e é a mesma
            // UMA IDENTIDADE TEM UMA E UMA SÓ CHAVE DE CONVERSA (#147).
            //
            // A x25519 é derivada DETERMINISTICAMENTE da mesma semente que a Ed25519 (ver o
            // `crypto.rs`): para uma dada chave pública de identidade existe exactamente uma
            // prekey válida. Portanto duas prekeys diferentes assinadas pela mesma identidade
            // não são uma actualização — são um facto que precisa de explicação.
            //
            // Isto substituía-a em silêncio. A assinatura confere (é ele que a assina), a
            // conversa passa a usar a chave nova, e tudo continua a funcionar — que é
            // exactamente a forma de uma troca de chaves não se notar. As duas leituras
            // possíveis são «a identidade dele foi comprometida» e «ele corre uma versão que
            // deriva a chave de outra maneira», e as duas merecem ser ditas em vez de
            // resolvidas por omissão.
            //
            // Guarda-se a ANTIGA e recusa-se a nova: a conversa que já existe continua a
            // decifrar, e a decisão de aceitar a chave nova fica para quem consegue ligar
            // para a pessoa e perguntar.
            Some(_) => {
                drop(m);
                bail!(
                    "a chave de conversa de {} mudou — ou a identidade dele foi comprometida, \
                     ou ele está numa versão incompatível. Fico com a antiga; fala com ele por \
                     outro caminho antes de aceitar a nova.",
                    &peer[..8.min(peer.len())]
                );
            }
            None => {}
        }
        m.insert(peer.to_string(), x_pub.to_string());
        drop(m);
        self.gravar_indice()
    }

    pub fn e_amigo(&self, chave: &str) -> bool {
        self.amigos
            .lock()
            .map(|a| a.iter().any(|x| x.chave == chave))
            .unwrap_or(false)
    }

    /// Se recuso tudo o que vem desta chave.
    ///
    /// Consultado à porta: uma ligação de alguém bloqueado fecha-se antes de dizer «olá», e
    /// por isso ele não consegue distinguir isto de eu estar desligado.
    pub fn e_bloqueado(&self, chave: &str) -> bool {
        self.bloqueados
            .lock()
            .map(|b| b.iter().any(|x| x == chave))
            .unwrap_or(false)
    }

    pub fn bloquear(&self, chave: &str, sim: bool) -> Result<()> {
        let chave = chave.trim().to_lowercase();
        hex32(&chave).map_err(|_| anyhow!("isso não é uma chave"))?;
        if chave == self.minha_chave() {
            bail!("essa chave é a tua");
        }
        {
            let mut b = self.bloqueados.lock().unwrap();
            b.retain(|x| x != &chave);
            if sim {
                b.push(chave.clone());
            }
        }
        // Bloquear é também deixar de ser amigo. Ter alguém nas duas listas seria a app a
        // dizer duas coisas contrárias sobre a mesma pessoa, e uma delas ia ganhar em
        // silêncio.
        if sim {
            self.amigos.lock().unwrap().retain(|x| x.chave != chave);
        }
        self.gravar_indice()
    }

    /// Se esta pessoa pode abrir-me uma conversa, segundo a política escolhida.
    pub fn pode_escrever_me(&self, chave: &str) -> bool {
        if self.e_bloqueado(chave) {
            return false;
        }
        // O valor COPIADO, e o lock largado antes de se pedir outro.
        //
        // Com `match *self.quem_escreve.lock()...` o guard sobrevive até ao fim do `match`,
        // e lá dentro pedem-se `amigos` e `servidores`. O `gravar_indice` toma-os pela ordem
        // contrária — e dois threads com ordens contrárias param os dois, para sempre, sem
        // um erro em lado nenhum. A app congelava, e só com a política em Amigos ou Salas.
        //
        // Copiar e largar faz esta função nunca segurar mais do que um lock de cada vez, e
        // aí não há ciclo possível seja qual for a ordem dos outros.
        let politica = *self.quem_escreve.lock().unwrap();
        match politica {
            QuemEscreve::Todos => true,
            QuemEscreve::Amigos => self.e_amigo(chave),
            QuemEscreve::Salas => {
                self.e_amigo(chave)
                    || self
                        .servidores
                        .lock()
                        .map(|s| {
                            s.values()
                                .filter(|srv| srv.com.is_none())
                                .any(|srv| srv.peers.iter().any(|p| p == chave))
                        })
                        .unwrap_or(false)
            }
        }
    }

    /// Põe alguém na lista. Guardar o nome que EU lhe dou, e não o que ele diz chamar-se.
    pub fn adicionar_amigo(&self, chave: &str, nome: &str) -> Result<()> {
        let chave = chave.trim().to_lowercase();
        // Validar a chave AQUI e não mais tarde: uma entrada invalida na lista seria uma
        // pessoa que o vigia tenta ligar para sempre e nunca alcança.
        hex32(&chave)
            .map_err(|_| anyhow!("isso não é uma chave: são 64 caracteres hexadecimais"))?;
        if chave == self.minha_chave() {
            bail!("essa chave é a tua");
        }
        let nome = nome.trim();
        if nome.is_empty() {
            bail!("dá-lhe um nome — é por ele que o vais reconhecer");
        }
        {
            let mut a = self.amigos.lock().unwrap();
            if let Some(ja) = a.iter_mut().find(|x| x.chave == chave) {
                ja.nome = nome.to_string(); // renomear, e não duplicar
            } else {
                a.push(Amigo {
                    chave,
                    nome: nome.to_string(),
                    desde_ms: agora_ms(),
                    verificado: false,
                });
            }
        }
        self.gravar_indice()
    }

    pub fn remover_amigo(&self, chave: &str) -> Result<()> {
        self.amigos.lock().unwrap().retain(|x| x.chave != chave);
        self.gravar_indice()
    }

    /// Marca (ou desmarca) que a chave foi comparada por outro caminho.
    pub fn marcar_verificado(&self, chave: &str, verificado: bool) -> Result<()> {
        {
            let mut a = self.amigos.lock().unwrap();
            let Some(x) = a.iter_mut().find(|x| x.chave == chave) else {
                bail!("essa pessoa não está na tua lista");
            };
            x.verificado = verificado;
        }
        self.gravar_indice()
    }

    /// Abre — ou reabre — a conversa privada com alguém.
    ///
    /// Não há convite e não há segredo a transportar: o id sai das duas chaves públicas e a
    /// chave sai do Diffie-Hellman entre elas. Os dois lados chegam ao mesmo sozinhos, e é
    /// por isso que uma conversa não se pode reencaminhar a terceiros como um convite.
    ///
    /// Por baixo é um servidor como os outros — o mesmo log assinado, a mesma cifra, o mesmo
    /// caminho de sincronização. A única diferença guardada é o `com`.
    /// Apaga uma conversa: do índice, do disco, e da lista de quem se disca (#87).
    ///
    /// # Porque é que isto tinha de existir
    ///
    /// Um pedido indesejado não tinha fim. A conversa aparecia na lista, o log ficava no
    /// disco, e o par ficava a ser discado — e a única coisa que se podia fazer era bloquear
    /// a pessoa, que é um gesto muito maior do que «não quero esta conversa».
    ///
    /// É irreversível de propósito e sem rede pelo meio: apaga-se aqui, e mais nada. Não se
    /// avisa o outro lado — dizer-lhe «apaguei a tua conversa» seria contar-lhe uma coisa que
    /// só me diz respeito a mim, e confirmava-lhe que a chave está viva.
    ///
    /// O ficheiro é renomeado para `.apagado` em vez de removido: um clique não deve ser
    /// capaz de destruir o único sítio onde uma conversa existe. Quem quiser mesmo perdê-la
    /// apaga o ficheiro à mão, e aí sabe o que está a fazer.
    pub fn apagar_conversa(&self, id: &str) -> Result<()> {
        {
            let mut s = self
                .servidores
                .lock()
                .map_err(|_| anyhow!("estado partido"))?;
            let Some(srv) = s.get(id) else {
                bail!("essa conversa não existe aqui");
            };
            if srv.com.is_none() {
                bail!("isso é um servidor, não uma conversa");
            }
            s.remove(id);
        }
        // O `lido` é indexado por «id/canal» e não por id: um `remove(id)` não apagava nada,
        // e uma conversa apagada que voltasse a nascer herdava as marcas de leitura da antiga.
        {
            let mut l = self.lido.lock().map_err(|_| anyhow!("estado partido"))?;
            let prefixo = format!("{id}/");
            l.retain(|k, _| k != id && !k.starts_with(&prefixo));
        }
        // O índice PRIMEIRO: se a gravação falhar, a conversa volta no arranque seguinte com
        // o log ao lado, que é melhor do que um índice sem chave e um ficheiro que já não
        // abre. Ver a ordem em `gravar_semente`.
        self.gravar_indice()?;
        let caminho = caminho_do_log(id);
        if caminho.exists() {
            let _ = std::fs::rename(&caminho, caminho.with_extension("apagado"));
        }
        Ok(())
    }

    pub fn abrir_conversa(&self, peer: &str) -> Result<String> {
        let minha = self.minha_chave();
        if peer == minha {
            bail!("essa chave é a tua");
        }
        let eu = hex32(&minha)?;
        let ele = hex32(peer)?;
        let id = HEXLOWER.encode(&crypto::id_da_conversa(&eu, &ele));

        if self.servidores.lock().unwrap().contains_key(&id) {
            return Ok(id); // já existe; abrir duas vezes é o mesmo que abrir uma
        }

        let x_hex = self
            .prekeys
            .lock()
            .unwrap()
            .get(peer)
            .cloned()
            .ok_or_else(|| {
                anyhow!(
                    "ainda não sei a chave de conversa desta pessoa — é preciso terem estado                      ligados pelo menos uma vez"
                )
            })?;
        let chave = crypto::session_key(
            &self.ident.x_secret,
            &x25519_dalek::PublicKey::from(hex32(&x_hex)?),
            &eu,
            &ele,
        )?;

        let log = blog::Log::load(caminho_do_log(&id))?;
        self.servidores.lock().unwrap().insert(
            id.clone(),
            Servidor {
                id: id.clone(),
                chave,
                log,
                // O outro é o único par desta conversa, e já é conhecido desde o início:
                // não há aqui o passo de "dar-se a conhecer" que os servidores têm.
                peers: vec![peer.to_string()],
                convidou: None,
                com: Some(peer.to_string()),
                provados: Default::default(),
            },
        );
        self.gravar_indice()?;
        Ok(id)
    }

    /// A chave com que uma sala/canal é identificado no mapa de leitura.
    ///
    /// Numa função e não escrita nos dois sítios: uma chave composta escrita à mão em dois
    /// lados é uma chave que um dia deixa de ser a mesma nos dois lados, e o sintoma seria
    /// «o não lido nunca desaparece», sem um erro em lado nenhum.
    pub fn chave_de_leitura(servidor: &str, canal: &str) -> String {
        format!("{servidor}/{canal}")
    }

    /// Marca um canal como lido até àquele instante. Devolve `true` se mudou alguma coisa.
    ///
    /// Só avança, nunca recua: se já estava lido mais à frente (por exemplo por a app estar
    /// aberta noutra janela), uma marcação mais antiga não pode desfazer isso.
    pub fn marcar_lido(&self, servidor: &str, canal: &str, ate_ms: i64) -> bool {
        let mut l = match self.lido.lock() {
            Ok(l) => l,
            Err(_) => return false,
        };
        let k = Self::chave_de_leitura(servidor, canal);
        match l.get(&k) {
            Some(anterior) if *anterior >= ate_ms => false,
            _ => {
                l.insert(k, ate_ms);
                true
            }
        }
    }

    pub fn lido_ate(&self, servidor: &str, canal: &str) -> i64 {
        self.lido
            .lock()
            .map(|l| {
                l.get(&Self::chave_de_leitura(servidor, canal))
                    .copied()
                    .unwrap_or(0)
            })
            .unwrap_or(0)
    }

    /// Congela as gravações do índice. Ver [`App::congelada`]. Uma vez, e não se desfaz — a
    /// app está prestes a reiniciar.
    pub fn congelar(&self) {
        self.congelada
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn gravar_indice(&self) -> Result<()> {
        if self.congelada.load(std::sync::atomic::Ordering::SeqCst) {
            bail!("a identidade foi restaurada; a app vai reiniciar e não grava mais nada");
        }
        // Recolher primeiro, e largar. Isto segurava `servidores` durante a serialização, a
        // cifra e a escrita no disco, enquanto ia pedindo os outros quatro — uma janela larga
        // para outro thread ficar preso do outro lado.
        let mut guardados: Vec<ServidorGuardado> = {
            let servidores = self.servidores.lock().unwrap();
            servidores
                .values()
                .map(|s| ServidorGuardado {
                    id: s.id.clone(),
                    chave: HEXLOWER.encode(&s.chave),
                    peers: s.peers.clone(),
                    com: s.com.clone(),
                    convidou: s.convidou.clone(),
                })
                .collect()
        };
        // E as que não abriram, tal e qual. Reservadas do arranque e nunca derivadas do mapa
        // vivo — é essa a diferença entre a chave sobreviver e ser apagada na primeira
        // gravação.
        guardados.extend(self.nao_abriram.lock().unwrap().iter().cloned());
        let indice = Indice {
            nome: self.nome.lock().unwrap().clone(),
            servidores: guardados,
            prekeys: self.prekeys.lock().unwrap().clone(),
            enderecos: self.enderecos.lock().unwrap().clone(),
            amigos: self.amigos.lock().unwrap().clone(),
            bloqueados: self.bloqueados.lock().unwrap().clone(),
            quem_escreve: *self.quem_escreve.lock().unwrap(),
            lido: self.lido.lock().unwrap().clone(),
            // O que uma versão mais recente escreveu e esta não conhece, devolvido intacto.
            resto: self.resto_do_indice.lock().unwrap().clone(),
        };
        // CIFRADO, sempre. O que aqui está são as chaves de todos os servidores.
        let claro = serde_json::to_vec(&indice)?;
        let (nonce, dados) = crypto::seal(&crypto::chave_do_indice(&self.semente), &claro)?;
        let cofre = Cofre {
            v: 1,
            nonce: HEXLOWER.encode(&nonce),
            dados: HEXLOWER.encode(&dados),
        };
        // TEMPORARIO, SYNC, RENAME -- e nao um `fs::write`.
        //
        // O `fs::write` abre com truncate: durante um instante o ficheiro tem tamanho zero e
        // o conteudo novo ainda nao la esta. Um corte de energia nessa janela deixa o
        // indice.json truncado, e o `ler_indice` nao tem recuperacao nenhuma -- poe o
        // ficheiro de lado e arranca com um indice VAZIO. Isso e perder as chaves de todos os
        // servidores de uma vez; os logs continuam no disco e passam a ser indecifraveis.
        //
        // A defesa ja existia no ficheiro do lado: o `Log::reescrever` do spike-common faz
        // exactamente isto, e ate tem o comentario a explicar porque. Eu li esse comentario
        // quando escrevi o log e nao o apliquei aqui -- no unico ficheiro cuja perda nao se
        // recupera.
        //
        // Enquanto o novo nao estiver inteiro e sincronizado, o antigo continua a ser o bom.
        //
        // E UMA ESCRITA DE CADA VEZ. Sem o lock, duas gravações simultâneas — uma da rede,
        // outra de um comando do Tauri — escreviam o MESMO `indice.novo` a partir do offset 0
        // de cada handle. Se os tamanhos diferissem, o que ficava era a cauda de uma colada ao
        // corpo da outra, e esse híbrido era renomeado por cima do índice: um cofre que não
        // decifra, e as chaves de todas as salas perdidas de uma vez. O lock é o último da
        // ordem, portanto não fecha ciclo com os outros.
        let _escrita = self.escrita_do_indice.lock().unwrap();
        let bytes = serde_json::to_string_pretty(&cofre)?.into_bytes();
        // UMA GERAÇÃO ANTERIOR (#17). O `escrever_atomico` já protege da queda a meio da
        // escrita, mas não de nada que torne o CONTEÚDO mau — um bug de serialização, um
        // sector que se degrada, um campo apagado por uma versão antiga. Este é o único
        // ficheiro cuja perda não se recupera de lado nenhum: os logs ficam, mas sem as
        // chaves são ruído. Guardar o índice actual como `.anterior` antes de o substituir
        // custa um `copy` e dá ao `ler_indice` um plano B. Duas gerações chegam.
        let destino = raiz().join("indice.json");
        if destino.exists() {
            let _ = std::fs::copy(&destino, destino.with_extension("anterior"));
        }
        escrever_atomico(&destino, &bytes)
    }
}

/// Escreve um ficheiro de forma ATOMICA: temporário, sync, rename.
///
/// O `fs::write` abre com truncate — durante um instante o ficheiro tem zero bytes e o
/// conteúdo novo ainda não lá está. Um corte de energia nessa janela deixa-o meio escrito.
/// Enquanto o novo não estiver inteiro e sincronizado, o antigo continua a ser o bom.
///
/// Está numa função só porque é a mesma defesa em três sítios (o índice, a semente, o que
/// vier a seguir), e escrevê-la três vezes é escrevê-la mal duas — foi assim que a semente
/// ficou de fora quando o índice já a tinha.
fn escrever_atomico(destino: &std::path::Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    // NOME UNICO POR ESCRITA, e não um `.novo` fixo.
    //
    // Com um nome partilhado, duas gravações ao mesmo tempo colidem no mesmo temporário: em
    // Windows a segunda falha com «sharing violation» e a gravação perde-se em silêncio (o
    // chamador fazia `let _ =`); noutros sistemas fica um híbrido que não decifra. Um nome
    // único por escrita fecha a corrida na origem — cada escrita tem o seu ficheiro, e o
    // `rename` atómico garante que o destino final é sempre um índice inteiro.
    static CONTADOR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let marca = CONTADOR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temporario = destino.with_extension(format!("novo-{}-{}", std::process::id(), marca));
    {
        let mut f = std::fs::File::create(&temporario)?;
        f.write_all(bytes)?;
        f.sync_data()?;
    }
    if let Err(e) = std::fs::rename(&temporario, destino) {
        let _ = std::fs::remove_file(&temporario);
        return Err(e.into());
    }
    Ok(())
}

/// Renomeia um ficheiro que não se conseguiu ler, para não voltar a matar o arranque.
///
/// Guarda-se em vez de se apagar: pode ser a única cópia de uma conversa. E com um CARIMBO
/// único no nome, porque `with_extension` fixo (`.estragado`) escrevia sempre o mesmo destino
/// — e em Windows o `rename` substitui sem avisar. O segundo ficheiro a falhar apagava a cópia
/// do primeiro, e um problema de disco raramente atinge um ficheiro só: o caso em que isto
/// interessa era precisamente o caso em que destruía a prova.
fn pos_de_lado(p: &std::path::Path) {
    let etiqueta = format!(
        "{}.estragado-{}",
        p.file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default(),
        agora_ms()
    );
    let mut destino = p.with_file_name(&etiqueta);
    // No caso improvável de dois no mesmo milissegundo, um contador em vez de sobrescrever.
    let mut n = 1u32;
    while destino.exists() {
        destino = p.with_file_name(format!("{etiqueta}-{n}"));
        n += 1;
    }
    match std::fs::rename(p, &destino) {
        Ok(()) => eprintln!("[dados] guardado como {}", destino.display()),
        Err(e) => eprintln!("[dados] não consegui pôr de lado o ficheiro: {e}"),
    }
}

/// O `indice.json` cifrado. Fica em JSON com o conteúdo em hex — em vez de bytes crus —
/// para continuar a ser um ficheiro de texto que se abre e se percebe: vê-se que está
/// cifrado, vê-se a versão do formato, e não se vê chave nenhuma.
/// A versão do formato do índice que ESTA versão do Bruma sabe escrever.
///
/// O `Cofre.v` era escrito e nunca lido. Ler serve para uma coisa concreta: se o índice foi
/// escrito por uma versão mais recente, LER pode (o `flatten` preserva o que não se conhece),
/// mas ESCREVER por cima não — seria a versão antiga a decidir o formato do futuro.
const VERSAO_DO_INDICE: u8 = 1;

#[derive(Serialize, Deserialize)]
struct Cofre {
    v: u8,
    nonce: String,
    dados: String,
}

/// Abre um `Cofre` já parseado, com um motivo LEGÍVEL para cada forma de falhar.
///
/// A distinção que interessa a quem lê o registo: `open` a falhar quase sempre quer dizer
/// «este índice é de outra identidade» (a semente mudou, ou uma restauração deixou o índice
/// velho ao lado da chave nova); os outros são corrupção do formato.
fn abrir_cofre(cofre: &Cofre, semente: &[u8; 32]) -> std::result::Result<Indice, String> {
    // LER PODE, ESCREVER POR CIMA NÃO.
    //
    // O `Cofre.v` era escrito e nunca lido. Se o índice foi escrito por uma versão mais
    // recente, o `flatten` do `Indice::resto` garante que se lê sem perder nada — mas gravar
    // por cima seria esta versão a decidir o formato do futuro. Marca-se, e o `gravar_indice`
    // recusa com uma mensagem que diz o que fazer.
    if cofre.v > VERSAO_DO_INDICE {
        return Err(format!(
            "o índice foi escrito por uma versão mais recente do Bruma (formato {} contra {});              actualiza antes de continuar",
            cofre.v, VERSAO_DO_INDICE
        ));
    }
    let nonce = hex24(&cofre.nonce).map_err(|_| "o nonce do índice está corrompido".to_string())?;
    let dados = HEXLOWER
        .decode(cofre.dados.as_bytes())
        .map_err(|_| "os dados do índice estão corrompidos (hex mal formado)".to_string())?;
    let claro = crypto::open(&crypto::chave_do_indice(semente), &nonce, &dados)
        .map_err(|_| "o índice deste computador pertence a outra identidade".to_string())?;
    serde_json::from_slice(&claro).map_err(|e| format!("o índice decifrou mas não se leu: {e}"))
}

fn ler_indice(raiz: &std::path::Path, semente: &[u8; 32]) -> Result<(Indice, bool)> {
    let p = raiz.join("indice.json");
    if !p.exists() {
        return Ok((Indice::default(), false));
    }
    let bruto = std::fs::read_to_string(&p)?;

    // Cifrado é o formato de hoje.
    //
    // Um Cofre bem formado que falha a decifrar ou a parsear NÃO mata a app. Antes, qualquer
    // `?` aqui subia até ao `.expect()` do main e a janela abria, piscava e desaparecia —
    // exactamente a avaria que o comentário do `arrancar` diz ter fechado para os servidores,
    // e que ficou aberta um nível acima, no único ficheiro que não se recupera.
    if let Ok(cofre) = serde_json::from_str::<Cofre>(&bruto) {
        match abrir_cofre(&cofre, semente) {
            Ok(i) => return Ok((i, false)),
            Err(motivo) => {
                eprintln!("[dados] o índice não abriu: {motivo}. A tentar a geração anterior.");
                // PLANO B: a geração anterior (#17). Se o actual se degradou mas o `.anterior`
                // ainda abre, recua-se uma geração — perde-se no máximo a última amizade ou a
                // última prekey, em vez de as chaves todas.
                let ant = raiz.join("indice.json.anterior");
                if let Ok(bruto_ant) = std::fs::read_to_string(&ant) {
                    if let Ok(cofre_ant) = serde_json::from_str::<Cofre>(&bruto_ant) {
                        if let Ok(i) = abrir_cofre(&cofre_ant, semente) {
                            eprintln!(
                                "[dados] recuei uma geração do índice (indice.json.anterior)."
                            );
                            pos_de_lado(&p);
                            return Ok((i, true));
                        }
                    }
                }
                // Nem o actual nem o anterior. Arranca-se vazio, com a razão certa — e a razão
                // distingue «este índice é de outra identidade» de «está corrompido», que são
                // problemas diferentes para quem os lê.
                eprintln!("[dados] {motivo}; a começar vazio. O ficheiro ficou guardado ao lado.");
                pos_de_lado(&p);
                return Ok((Indice::default(), false));
            }
        }
    }

    // Em texto simples é o formato ANTIGO: lê-se, e a primeira gravação passa-o a cifrado.
    // Não se converte aqui de propósito — converter durante a leitura seria escrever no
    // disco antes de a app estar de pé, e é nesse momento que menos se quer surpresas.
    if let Ok(i) = serde_json::from_str::<Indice>(&bruto) {
        return Ok((i, true));
    }

    match Err::<Indice, _>(anyhow!("não é nem cifrado nem do formato antigo")) {
        Ok(i) => Ok((i, false)),
        Err(e) => {
            // O índice guarda o NOME e as chaves dos servidores. Sem ele não há nada para
            // abrir — mas há uma diferença enorme entre "a app não abre" e "a app abre
            // vazia e diz o que aconteceu". Escolhe-se a segunda.
            eprintln!("[dados] o índice está estragado ({e}); a começar vazio");
            pos_de_lado(&p);
            Ok((Indice::default(), false))
        }
    }
}

/// Grava uma semente com soma de controlo (36 bytes) de forma atómica.
///
/// A mesma forma que o `semente_ou_cria` usa ao criar — extraída para o `restaurar_identidade`
/// não a escrever à mão com um `fs::write` cru, que foi como a semente restaurada ficava sem
/// checksum e sem durabilidade, ao contrário da criada.
pub fn gravar_semente(p: &std::path::Path, semente: &[u8; 32]) -> Result<()> {
    let mut com_soma = Vec::with_capacity(36);
    com_soma.extend_from_slice(semente);
    com_soma.extend_from_slice(&crypto::soma_de_controlo(semente));
    escrever_atomico(p, &com_soma)
}

fn semente_ou_cria(p: &std::path::Path) -> Result<[u8; 32]> {
    if let Ok(raw) = std::fs::read(p) {
        // 32 bytes: FORMATO ANTIGO, aceite tal e qual. Convertido para 36 na próxima escrita
        // — não se reescreve aqui, à leitura, porque a leitura tem de poder ser só de leitura.
        if raw.len() == 32 {
            let mut s = [0u8; 32];
            s.copy_from_slice(&raw);
            return Ok(s);
        }
        // 36 bytes: os 32 da semente mais 4 de soma. Um bit virado no ficheiro deixa de trocar
        // a pessoa em silêncio — dá um erro que diz o que aconteceu e aponta para as 24
        // palavras. NÃO se arranca como outra pessoa: essa é a falha irreversível.
        if raw.len() == 36 {
            let mut s = [0u8; 32];
            s.copy_from_slice(&raw[..32]);
            if crypto::soma_de_controlo(&s) == raw[32..] {
                return Ok(s);
            }
            bail!(
                "o ficheiro da tua identidade ({}) está corrompido — um bit trocado.                  Tens as 24 palavras? Restaura a identidade com elas.",
                p.display()
            );
        }
        bail!(
            "{} existe mas não tem 32 nem 36 bytes — não é uma identidade do Bruma",
            p.display()
        );
    }
    // Criar: 36 bytes, e ATOMICO. Era o único ficheiro cuja perda é irreversível e era escrito
    // com menos cuidado do que o índice — um `fs::write` cru, sem sync nem rename.
    let mut s = [0u8; 32];
    getrandom::getrandom(&mut s).map_err(|e| anyhow!("rng: {e}"))?;
    gravar_semente(p, &s)?;
    Ok(s)
}

pub fn agora_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn hex32(s: &str) -> Result<[u8; 32]> {
    let v = HEXLOWER.decode(s.as_bytes())?;
    if v.len() != 32 {
        bail!("esperava 32 bytes");
    }
    let mut o = [0u8; 32];
    o.copy_from_slice(&v);
    Ok(o)
}

fn hex24(s: &str) -> Result<[u8; 24]> {
    let v = HEXLOWER.decode(s.as_bytes())?;
    if v.len() != 24 {
        bail!("esperava 24 bytes");
    }
    let mut o = [0u8; 24];
    o.copy_from_slice(&v);
    Ok(o)
}

pub fn nova_chave_de_servidor() -> Result<[u8; 32]> {
    let mut k = [0u8; 32];
    getrandom::getrandom(&mut k).map_err(|e| anyhow!("rng: {e}"))?;
    Ok(k)
}

/// Um id de servidor é um nome de ficheiro, e tem de ser tratado como tal.
///
/// O id vinha de um **convite**, que é JSON em base32 sem assinatura nenhuma — quem escreve
/// o convite escolhe o que lá está. E ia directo para
/// `raiz().join("servidores").join(format!("{id}.json"))`. O `PathBuf::join` com um caminho
/// ABSOLUTO deita fora o prefixo e fica só com o absoluto: um convite com
/// `servidor = "C:/Users/x/AppData/Roaming/Microsoft/Windows/Start Menu/Programs/Startup/z"`
/// fazia a app criar e escrever um ficheiro na pasta de arranque do Windows. Com `..` chegava
/// ao mesmo sítio pelo caminho longo.
///
/// Os ids que a app gera são 32 caracteres hex. Aceitar exactamente isso — e não «tentar
/// limpar» o que vier — é a única forma que não tem casos esquecidos: não há aqui uma lista
/// de coisas proibidas, há uma lista de coisas permitidas.
pub fn id_de_servidor_valido(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Uma chave pública em hex minúsculo: 64 caracteres, nem mais nem menos.
pub fn chave_valida(k: &str) -> bool {
    k.len() == 64
        && k.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

pub fn caminho_do_log(id: &str) -> PathBuf {
    raiz().join("servidores").join(format!("{id}.json"))
}

#[cfg(test)]
mod testes {
    use super::*;

    /// UM CIPHERTEXT COPIADO JÁ NÃO TORNA NINGUÉM MEMBRO (#197).
    ///
    /// # A avaria que isto fecha, e que foi medida antes de ser corrigida
    ///
    /// A cifra não usava dados associados: os mesmos bytes de `ciphertext` decifravam na
    /// mesma sala esteja em que entrada estivessem. A assinatura cobre
    /// `author‖ts‖prev‖nonce‖ct`, mas quem assina é quem quiser — ela prova quem MONTOU a
    /// entrada, não quem escreveu o conteúdo.
    ///
    /// Portanto: alguém que obtivesse o log cifrado de uma sala — do disco, de uma cópia de
    /// segurança, de um `Sync` antigo — mas NÃO tivesse a chave, copiava um `ciphertext` de
    /// lá, assinava-o com a sua identidade e mandava-o. Do nosso lado decifrava (nós temos a
    /// chave) e o autor entrava em `autores_provados` — a lista que dá direitos de sala.
    /// **Copiar bytes que não se conseguem ler dava direitos de membro.**
    ///
    /// Este teste existiu primeiro na forma contrária: afirmava que o ataque FUNCIONAVA, para
    /// a decisão sobre o formato ser tomada em cima de um facto e não de um receio. Inverteu-o
    /// a correcção, e é isso que o torna a prova — a mesma montagem, o resultado ao contrário.
    #[test]
    fn um_ciphertext_copiado_nao_torna_ninguem_membro() {
        let chave = [11u8; 32];
        let membro = crypto::Identity::from_seed(&[1u8; 32]);
        let forasteiro = crypto::Identity::from_seed(&[2u8; 32]);

        let tmp = |n: &str| {
            let c =
                std::env::temp_dir().join(format!("bruma-aad-{n}-{}.jsonl", std::process::id()));
            let _ = std::fs::remove_file(&c);
            c
        };
        let sala_nova = |caminho: &std::path::Path| {
            Servidor::novo(
                "sala".into(),
                chave,
                blog::Log::load(caminho).unwrap(),
                Vec::new(),
                None,
                None,
            )
        };

        // Selada COM o autor, à mão: é o que uma versão com o `ESCREVER_PRESO_AO_AUTOR`
        // ligado produz. Esta versão ainda escreve à moda antiga (ver a nota lá em cima),
        // mas a DEFESA — que é do lado da leitura — já está de pé, e é ela que se mede aqui.
        let claro = serde_json::to_vec(&crate::modelo::Carga::Mensagem {
            canal: "geral".into(),
            texto: "olá a todos".into(),
        })
        .unwrap();
        let (nonce_o, ct_o) = crypto::seal_com(
            &chave,
            &claro,
            membro.signing.verifying_key().as_bytes().as_slice(),
        )
        .unwrap();

        let c1 = tmp("a");
        let mut log_dela = blog::Log::load(&c1).unwrap();
        let original = log_dela
            .append_local(&membro.signing, nonce_o, ct_o, 1000)
            .unwrap();

        // O forasteiro copia o `nonce` e o `ciphertext` — que é tudo o que ele vê no log — e
        // monta uma entrada SUA com eles. Não tem a chave da sala, e não precisa dela.
        let c2 = tmp("b");
        let mut log_dele = blog::Log::load(&c2).unwrap();
        let nonce = hex24(&original.nonce).unwrap();
        let ct = HEXLOWER.decode(original.ciphertext.as_bytes()).unwrap();
        let copiada = log_dele
            .append_local(&forasteiro.signing, nonce, ct, 12345)
            .unwrap();

        let c3 = tmp("c");
        let mut nossa = sala_nova(&c3);
        let (entraram, recusadas) = nossa.merge_contado(vec![copiada]).unwrap();

        let dele = HEXLOWER.encode(forasteiro.signing.verifying_key().as_bytes());
        assert_eq!(
            (entraram, recusadas),
            (0, 1),
            "a entrada copiada é recusada, e conta como lixo"
        );
        assert!(
            !nossa.autores_provados().contains(&dele),
            "copiar um ciphertext não pode dar direitos de membro"
        );

        // E A OUTRA METADE, que é a que impede a correcção de ser uma avaria: a entrada
        // LEGÍTIMA continua a entrar, e a ler-se.
        let c4 = tmp("d");
        let mut outra = sala_nova(&c4);
        let (entraram, recusadas) = outra.merge_contado(vec![original]).unwrap();
        assert_eq!((entraram, recusadas), (1, 0), "a original entra sem custo");
        assert_eq!(
            outra.mensagens("geral").len(),
            1,
            "e lê-se: a carga decifra com o autor certo"
        );

        for c in [c1, c2, c3, c4] {
            let _ = std::fs::remove_file(&c);
        }
    }

    /// O QUE ESTA VERSÃO ESCREVE CONTINUA A SER LEGÍVEL PELA ANTERIOR (#197).
    ///
    /// # Porque é que este teste é o mais importante dos três
    ///
    /// Os outros dois provam que a defesa funciona e que o passado continua a abrir. Este
    /// prova que **a outra máquina continua a receber as minhas mensagens** — e é o que
    /// impede esta correcção de ser uma avaria muito pior do que o defeito que fecha.
    ///
    /// Uma entrada selada com o autor não abre numa versão que não conheça o truque: o
    /// `merge_verificado` da v0.18.3 filtra por um `crypto::open` sem dados associados, e o
    /// que não abre é deitado fora sem uma linha em lado nenhum. Se esta versão já escrevesse
    /// assim, cada mensagem minha desaparecia no caminho para quem ainda não actualizou.
    ///
    /// Por isso o `ESCREVER_PRESO_AO_AUTOR` está a `false`, e este teste é o portão que o
    /// segura: no dia em que alguém o puser a `true`, isto falha e obriga a perguntar se as
    /// duas casas já estão na mesma versão. É uma pergunta que tem de ser feita, e um teste
    /// vermelho é a única forma de garantir que é.
    #[test]
    fn a_versao_anterior_continua_a_ler_o_que_esta_escreve() {
        let chave = [13u8; 32];
        let autor = crypto::Identity::from_seed(&[4u8; 32]);
        let caminho =
            std::env::temp_dir().join(format!("bruma-compat-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&caminho);

        let mut srv = Servidor::novo(
            "sala".into(),
            chave,
            blog::Log::load(&caminho).unwrap(),
            Vec::new(),
            None,
            None,
        );
        let e = srv
            .escrever(
                &autor.signing,
                &crate::modelo::Carga::Mensagem {
                    canal: "geral".into(),
                    texto: "a amiga tem de ler isto".into(),
                },
            )
            .unwrap();

        // Exactamente o que a v0.18.3 faz: `crypto::open` sem dados associados nenhuns.
        let nonce = hex24(&e.nonce).unwrap();
        let ct = HEXLOWER.decode(e.ciphertext.as_bytes()).unwrap();
        let claro = crypto::open(&chave, &nonce, &ct).expect(
            "a versao anterior TEM de conseguir abrir isto -- se falhou, o              ESCREVER_PRESO_AO_AUTOR foi ligado antes de as duas casas actualizarem",
        );
        let carga: crate::modelo::Carga = serde_json::from_slice(&claro).unwrap();
        assert!(
            matches!(carga, crate::modelo::Carga::Mensagem { ref texto, .. } if texto.contains("amiga")),
            "e com o texto certo"
        );

        let _ = std::fs::remove_file(&caminho);
    }

    /// O HISTÓRICO ANTERIOR A ESTA VERSÃO CONTINUA A ABRIR (#197).
    ///
    /// A correcção só vale se não custar o passado. Uma entrada cifrada à moda antiga — sem
    /// dados associados — tem de continuar a decifrar, senão a actualização apagava do ecrã
    /// tudo o que já lá estava, em silêncio, que é a pior forma de falhar que este projecto
    /// conhece.
    ///
    /// E a leitura dupla não abre porta nenhuma: o AEAD faz a etiqueta cobrir o `aad`, portanto
    /// um `ciphertext` selado COM o autor **não abre sem ele**. Um atacante não pode escolher a
    /// via antiga para uma entrada nova — ela falha nos dois sentidos. O que continua copiável
    /// é só o que foi escrito antes desta versão: um conjunto fechado, que não cresce.
    #[test]
    fn o_historico_de_antes_do_aad_continua_a_abrir() {
        let chave = [12u8; 32];
        let autor = crypto::Identity::from_seed(&[3u8; 32]);
        let caminho =
            std::env::temp_dir().join(format!("bruma-aad-velho-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&caminho);

        // Cifrada como a v0.18.3 e anteriores cifravam: sem AAD nenhum.
        let claro = serde_json::to_vec(&crate::modelo::Carga::Mensagem {
            canal: "geral".into(),
            texto: "isto é de antes".into(),
        })
        .unwrap();
        let (nonce, ct) = crypto::seal(&chave, &claro).unwrap();

        let mut log = blog::Log::load(&caminho).unwrap();
        let antiga = log.append_local(&autor.signing, nonce, ct, 1000).unwrap();

        let outro =
            std::env::temp_dir().join(format!("bruma-aad-velho2-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&outro);
        let mut srv = Servidor::novo(
            "sala".into(),
            chave,
            blog::Log::load(&outro).unwrap(),
            Vec::new(),
            None,
            None,
        );
        let (entraram, recusadas) = srv.merge_contado(vec![antiga]).unwrap();
        assert_eq!(
            (entraram, recusadas),
            (1, 0),
            "uma entrada de antes do AAD entra na mesma"
        );
        let msgs = srv.mensagens("geral");
        assert_eq!(msgs.len(), 1, "e lê-se");
        assert!(
            msgs[0].texto.contains("de antes"),
            "com o texto certo: {:?}",
            msgs[0].texto
        );

        let _ = std::fs::remove_file(&caminho);
        let _ = std::fs::remove_file(&outro);
    }

    /// O QUE SE CONTA É O LIXO, E NÃO O VOLUME (#85).
    ///
    /// # Porque é que a distinção é a defesa toda
    ///
    /// À entrada não havia porteiro nenhum: um `Sync` de um estranho com o id de uma sala
    /// minha ia direito ao merge, e cada entrada custava uma verificação de assinatura e uma
    /// tentativa de decifragem — com o mutex dos servidores segurado, portanto com a app
    /// congelada.
    ///
    /// A tentação é pôr um tecto no NÚMERO de entradas. Seria uma avaria: um par legítimo com
    /// um ano de conversa manda milhares, e cortá-lo dá exactamente o sintoma que este
    /// projecto passou versões a caçar — sincronização que pára sem dizer porquê.
    ///
    /// O que separa os dois casos não é o volume: é a chave. Quem pertence à sala tem-na, e
    /// TUDO o que manda decifra. Quem não pertence não tem, e NADA do que manda decifra. Este
    /// teste é essa afirmação: mil entradas boas custam zero do orçamento, e as más custam
    /// todas.
    #[test]
    fn o_orcamento_conta_o_lixo_e_nao_o_volume() {
        let chave = [9u8; 32];
        let eu = crypto::Identity::from_seed(&[1u8; 32]);
        let intruso = crypto::Identity::from_seed(&[2u8; 32]);

        let caminho = std::env::temp_dir().join(format!("bruma-orc-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&caminho);
        let mut srv = Servidor::novo(
            "sala".into(),
            chave,
            blog::Log::load(&caminho).unwrap(),
            Vec::new(),
            None,
            None,
        );

        // MIL entradas legítimas: cifradas com a chave da sala, assinadas por quem pertence.
        let mut boas = Vec::new();
        {
            let outro_caminho =
                std::env::temp_dir().join(format!("bruma-orc-b-{}.jsonl", std::process::id()));
            let _ = std::fs::remove_file(&outro_caminho);
            let mut fonte = Servidor::novo(
                "sala".into(),
                chave,
                blog::Log::load(&outro_caminho).unwrap(),
                Vec::new(),
                None,
                None,
            );
            for i in 0..1000 {
                boas.push(
                    fonte
                        .escrever(
                            &eu.signing,
                            &crate::modelo::Carga::Mensagem {
                                canal: "geral".into(),
                                texto: format!("mensagem {i}"),
                            },
                        )
                        .unwrap(),
                );
            }
            let _ = std::fs::remove_file(&outro_caminho);
        }
        let (novas, recusadas) = srv.merge_contado(boas).unwrap();
        assert_eq!(novas, 1000, "todas as legítimas entram");
        assert_eq!(
            recusadas, 0,
            "e nenhuma gasta orçamento: quem pertence tem a chave"
        );

        // E agora o lixo: assinado (qualquer pessoa assina o que quiser) mas cifrado com
        // OUTRA chave. É o que um estranho consegue fabricar.
        let mut lixo = Vec::new();
        {
            let sujo =
                std::env::temp_dir().join(format!("bruma-orc-l-{}.jsonl", std::process::id()));
            let _ = std::fs::remove_file(&sujo);
            let mut fonte = Servidor::novo(
                "outra".into(),
                [3u8; 32], // chave que a minha sala não tem
                blog::Log::load(&sujo).unwrap(),
                Vec::new(),
                None,
                None,
            );
            for i in 0..40 {
                lixo.push(
                    fonte
                        .escrever(
                            &intruso.signing,
                            &crate::modelo::Carga::Mensagem {
                                canal: "x".into(),
                                texto: format!("lixo {i}"),
                            },
                        )
                        .unwrap(),
                );
            }
            let _ = std::fs::remove_file(&sujo);
        }
        let (novas, recusadas) = srv.merge_contado(lixo).unwrap();
        assert_eq!(novas, 0, "nada do intruso entra");
        assert_eq!(recusadas, 40, "e tudo o que ele mandou gasta orçamento");

        // E o intruso não passou a constar como autor da sala.
        assert!(
            !srv.autores_provados()
                .contains(&HEXLOWER.encode(intruso.signing.verifying_key().as_bytes())),
            "quem não decifra não se torna membro"
        );
        let _ = std::fs::remove_file(&caminho);
    }
    use std::sync::Arc;

    /// `BRUMA_DADOS` é uma variável de ambiente GLOBAL ao processo, e vários testes definem-na
    /// para pastas diferentes. Correm em paralelo por omissão, e o `gravar_indice` relê a
    /// variável a cada escrita — sem serialização, um teste gravava na pasta de outro e a
    /// gravação falhava. Passou no meu portátil e falhou no runner do CI, que é o que os
    /// testes de paralelismo fazem. Cada teste que mexe na variável segura este lock do
    /// princípio ao fim.
    static DADOS_DE_TESTE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    fn trava_dados() -> std::sync::MutexGuard<'static, ()> {
        DADOS_DE_TESTE.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Constrói um `indice.json` cifrado com uma semente, com um nome de perfil identificável.
    fn cofre_com(dir: &std::path::Path, ficheiro: &str, semente: &[u8; 32], nome: &str) {
        let indice = Indice {
            nome: nome.to_string(),
            ..Default::default()
        };
        let claro = serde_json::to_vec(&indice).unwrap();
        let (nonce, dados) = crypto::seal(&crypto::chave_do_indice(semente), &claro).unwrap();
        let cofre = Cofre {
            v: 1,
            nonce: HEXLOWER.encode(&nonce),
            dados: HEXLOWER.encode(&dados),
        };
        std::fs::write(
            dir.join(ficheiro),
            serde_json::to_string_pretty(&cofre).unwrap(),
        )
        .unwrap();
    }

    /// Um campo escrito por uma versão mais recente sobrevive a esta versão o ler e gravar.
    ///
    /// Sem o `flatten`, o serde descartava-o e a primeira gravação apagava-o para sempre —
    /// e com a rotação de chave no horizonte, isso é perda de chaves, não de comodidade.
    #[test]
    fn campo_de_versao_futura_sobrevive_a_ida_e_volta() {
        // Um índice como a v0.19 o escreveria: campos conhecidos mais um que esta não conhece.
        let json = br#"{"nome":"eu","servidores":[],"chaves_antigas":{"sala1":"abc"}}"#;
        let indice: Indice = serde_json::from_slice(json).expect("tem de ler");
        assert_eq!(indice.nome, "eu", "os conhecidos leem-se");
        assert!(
            indice.resto.contains_key("chaves_antigas"),
            "o campo desconhecido tinha de ser guardado, e não descartado"
        );

        // E ao voltar a escrever, sai intacto.
        let volta = serde_json::to_string(&indice).unwrap();
        assert!(
            volta.contains("chaves_antigas") && volta.contains("abc"),
            "o campo desconhecido tinha de ser reescrito intacto: {volta}"
        );
    }

    /// Um índice de formato MAIS RECENTE não se sobrescreve — lê-se, mas recusa-se gravar.
    #[test]
    fn indice_de_formato_futuro_e_recusado() {
        let semente = [8u8; 32];
        let indice = Indice {
            nome: "do futuro".into(),
            ..Default::default()
        };
        let claro = serde_json::to_vec(&indice).unwrap();
        let (nonce, dados) = crypto::seal(&crypto::chave_do_indice(&semente), &claro).unwrap();

        // O mesmo conteúdo, com a versão de formato desta app: abre.
        let agora = Cofre {
            v: VERSAO_DO_INDICE,
            nonce: HEXLOWER.encode(&nonce),
            dados: HEXLOWER.encode(&dados),
        };
        assert!(
            abrir_cofre(&agora, &semente).is_ok(),
            "o formato de hoje tem de abrir"
        );

        // Com uma versão de formato futura: recusa, com a razão certa.
        let futuro = Cofre {
            v: VERSAO_DO_INDICE + 1,
            nonce: agora.nonce.clone(),
            dados: agora.dados.clone(),
        };
        let e = abrir_cofre(&futuro, &semente).unwrap_err();
        assert!(
            e.contains("versão mais recente"),
            "a razão tem de dizer o que se passa: {e}"
        );
    }

    /// A raiz de produção é ABSOLUTA — um caminho absoluto não depende do cwd, que era o bug.
    ///
    /// O ramo antigo era `PathBuf::from("dados")`, relativo ao directório de trabalho: uma
    /// mudança de cwd a meio da sessão mandava as gravações para outra pasta. Estável ao cwd
    /// = absoluto.
    #[test]
    fn a_raiz_de_producao_e_absoluta() {
        assert!(
            resolver_raiz().is_absolute(),
            "a raiz de produção não pode ser relativa ao cwd"
        );
    }

    /// As mensagens não mudam o índice — logo, não há razão para o gravar por causa delas.
    ///
    /// É a justificação do #140: o índice guarda chaves, peers, amigos, marcas de leitura —
    /// nunca mensagens. Escrever mensagens num servidor e voltar a gravar o índice tem de
    /// produzir o mesmo conteúdo claro.
    #[test]
    fn mensagens_nao_mudam_o_indice() {
        let _guarda_dados = trava_dados();
        let dir = std::env::temp_dir().join(format!("bruma-msg-idx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        unsafe { std::env::set_var("BRUMA_DADOS", &dir) };
        let app = App::arrancar().expect("arrancar");

        // Um servidor com uma chave conhecida, para poder escrever nele.
        let chave = [4u8; 32];
        let log = blog::Log::load(caminho_do_log("aa".repeat(16).as_str())).unwrap();
        let srv = Servidor::novo("aa".repeat(16), chave, log, vec![], None, None);
        app.servidores.lock().unwrap().insert("aa".repeat(16), srv);

        app.gravar_indice().unwrap();
        let (antes, _) = ler_indice(&dir, &app.semente).unwrap();
        let antes = serde_json::to_string(&antes).unwrap();

        // Escrever mensagens no log do servidor — o que muda os FICHEIROS, não o índice.
        {
            let mut sv = app.servidores.lock().unwrap();
            let srv = sv.get_mut(&"aa".repeat(16)).unwrap();
            for i in 0..5 {
                srv.escrever(
                    &app.ident.signing,
                    &Carga::Mensagem {
                        canal: "geral".into(),
                        texto: format!("m{i}"),
                    },
                )
                .unwrap();
            }
        }
        app.gravar_indice().unwrap();
        let (depois, _) = ler_indice(&dir, &app.semente).unwrap();
        let depois = serde_json::to_string(&depois).unwrap();

        assert_eq!(
            antes, depois,
            "as mensagens não podem mudar o conteúdo do índice"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Depois de congelada, a app recusa gravar o índice (#6, #16).
    ///
    /// É o que impede a corrupção entre restaurar a identidade e o processo reiniciar: a
    /// semente em memória ainda é a antiga, e uma gravação nesse intervalo deixava um índice
    /// que no arranque seguinte não abria.
    #[test]
    fn congelada_recusa_gravar() {
        let _guarda_dados = trava_dados();
        let dir = std::env::temp_dir().join(format!("bruma-congela-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        unsafe { std::env::set_var("BRUMA_DADOS", &dir) };
        let app = App::arrancar().expect("arrancar");

        app.gravar_indice().expect("antes de congelar, grava");
        app.congelar();
        assert!(app.gravar_indice().is_err(), "congelada não pode gravar");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Uma sala que não abre no arranque NÃO perde a chave na primeira gravação (#91, #129).
    ///
    /// O `gravar_indice` reconstrói a lista a partir do mapa vivo, e a sala má não está nele.
    /// Sem a guardar à parte, a próxima gravação — e grava-se por tudo — apagava-a do índice
    /// e com ela a chave, deixando o ficheiro no disco como ruído para sempre.
    #[test]
    fn sala_que_nao_abre_mantem_a_chave() {
        let _guarda_dados = trava_dados();
        let dir = std::env::temp_dir().join(format!("bruma-quarentena-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("servidores")).unwrap();
        unsafe { std::env::set_var("BRUMA_DADOS", &dir) };

        // Semente conhecida (36 bytes com soma), para poder ler o índice depois.
        let semente = [3u8; 32];
        let mut key = semente.to_vec();
        key.extend_from_slice(&crypto::soma_de_controlo(&semente));
        std::fs::write(dir.join("identidade.key"), &key).unwrap();

        // Índice com uma sala de chave INVÁLIDA (hex mau), cifrado com a semente.
        let chave_ma = "zz".repeat(32);
        let indice = Indice {
            servidores: vec![ServidorGuardado {
                id: "sala-que-nao-abre".into(),
                chave: chave_ma.clone(),
                peers: vec![],
                com: None,
                convidou: None,
            }],
            ..Default::default()
        };
        let claro = serde_json::to_vec(&indice).unwrap();
        let (nonce, dados) = crypto::seal(&crypto::chave_do_indice(&semente), &claro).unwrap();
        let cofre = Cofre {
            v: 1,
            nonce: HEXLOWER.encode(&nonce),
            dados: HEXLOWER.encode(&dados),
        };
        std::fs::write(
            dir.join("indice.json"),
            serde_json::to_string_pretty(&cofre).unwrap(),
        )
        .unwrap();

        // Arrancar: a sala má fica de fora do mapa (chave inválida), mas em quarentena.
        let app = App::arrancar().expect("arranca apesar da sala má");
        assert!(
            !app.servidores
                .lock()
                .unwrap()
                .contains_key("sala-que-nao-abre"),
            "a sala de chave inválida não entra no mapa vivo"
        );

        // Gravar o índice — é aqui que a chave se perdia.
        app.gravar_indice().unwrap();

        // Reler o índice: a sala má tem de continuar lá, com a chave intacta.
        let (relido, _) = ler_indice(&dir, &semente).unwrap();
        let achada = relido
            .servidores
            .iter()
            .find(|x| x.id == "sala-que-nao-abre");
        assert!(achada.is_some(), "a sala tinha de sobreviver à gravação");
        assert_eq!(achada.unwrap().chave, chave_ma, "com a chave intacta");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Um índice cifrado com OUTRA semente faz a app abrir vazia, não morrer (#5, #75).
    #[test]
    fn indice_de_outra_identidade_arranca_vazio() {
        let dir = std::env::temp_dir().join(format!("bruma-outra-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let minha = [1u8; 32];
        let outra = [2u8; 32];

        // Índice de outra identidade no disco.
        cofre_com(&dir, "indice.json", &outra, "não sou eu");

        // Ler com a MINHA semente: tem de arrancar vazio, NÃO dar erro fatal.
        let (indice, _) = ler_indice(&dir, &minha).expect("não pode matar a app");
        assert_eq!(indice.nome, "", "arranca vazio, não com o nome do outro");
        assert!(
            std::fs::read_dir(&dir).unwrap().any(|e| e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".estragado")),
            "o índice de outra identidade tinha de ficar guardado ao lado"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Índice actual degradado, mas a geração anterior abre: recua-se (#17).
    #[test]
    fn indice_corrompido_recua_uma_geracao() {
        let dir = std::env::temp_dir().join(format!("bruma-geracao-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let semente = [5u8; 32];

        // Geração anterior boa, actual corrompido (um Cofre válido cujo `dados` não decifra).
        cofre_com(&dir, "indice.json.anterior", &semente, "a geração de ontem");
        cofre_com(&dir, "indice.json", &semente, "a geração de hoje");
        let bruto = std::fs::read_to_string(dir.join("indice.json")).unwrap();
        let mut cofre: Cofre = serde_json::from_str(&bruto).unwrap();
        // Virar um byte no meio do hex de `dados`: continua a parsear como Cofre, mas o
        // `open` falha — que é o caso em que se deve tentar a geração anterior.
        let meio = cofre.dados.len() / 2;
        let b = &cofre.dados[meio..meio + 1];
        let trocado = if b == "a" { "b" } else { "a" };
        cofre.dados.replace_range(meio..meio + 1, trocado);
        std::fs::write(
            dir.join("indice.json"),
            serde_json::to_string_pretty(&cofre).unwrap(),
        )
        .unwrap();

        let (indice, _) = ler_indice(&dir, &semente).expect("não pode matar a app");
        assert_eq!(indice.nome, "a geração de ontem", "recuou para a anterior");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Uma escrita atómica não deixa temporário e põe lá o conteúdo certo.
    #[test]
    fn escrita_atomica_nao_deixa_rasto() {
        let dir = std::env::temp_dir().join(format!("bruma-atom-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let alvo = dir.join("x.dat");
        escrever_atomico(&alvo, b"conteudo").unwrap();
        assert_eq!(std::fs::read(&alvo).unwrap(), b"conteudo");
        assert!(
            !alvo.with_extension("novo").exists(),
            "o temporário tinha de desaparecer"
        );
        // Reescrever por cima também funciona.
        escrever_atomico(&alvo, b"outro").unwrap();
        assert_eq!(std::fs::read(&alvo).unwrap(), b"outro");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Pôr dois ficheiros de lado não apaga a cópia do primeiro.
    #[test]
    fn quarentena_nao_sobrescreve() {
        let dir = std::env::temp_dir().join(format!("bruma-quar-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("s.json");
        std::fs::write(&a, b"primeiro").unwrap();
        pos_de_lado(&a);
        std::fs::write(&a, b"segundo").unwrap();
        pos_de_lado(&a);
        // Os dois estragados têm de coexistir, com os dois conteúdos.
        let mut conteudos: Vec<Vec<u8>> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".estragado"))
            .map(|e| std::fs::read(e.path()).unwrap())
            .collect();
        conteudos.sort();
        assert_eq!(conteudos, vec![b"primeiro".to_vec(), b"segundo".to_vec()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A semente: 36 bytes com soma boa entra; soma má é recusada; 32 (antigo) entra.
    #[test]
    fn semente_com_soma_de_controlo() {
        let dir = std::env::temp_dir().join(format!("bruma-sem-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let alvo = dir.join("identidade.key");

        // Criada de raiz: 36 bytes, e relê-se igual.
        let criada = semente_ou_cria(&alvo).unwrap();
        assert_eq!(
            std::fs::read(&alvo).unwrap().len(),
            36,
            "tem de guardar 36 bytes"
        );
        assert_eq!(semente_ou_cria(&alvo).unwrap(), criada, "relê a mesma");

        // Um bit virado no ficheiro é RECUSADO, não aceite como outra pessoa.
        let mut raw = std::fs::read(&alvo).unwrap();
        raw[3] ^= 0x01;
        std::fs::write(&alvo, &raw).unwrap();
        assert!(
            semente_ou_cria(&alvo).is_err(),
            "um bit virado tinha de dar erro"
        );

        // Formato antigo de 32 bytes continua a ser aceite.
        std::fs::write(&alvo, [9u8; 32]).unwrap();
        assert_eq!(
            semente_ou_cria(&alvo).unwrap(),
            [9u8; 32],
            "32 bytes ainda entra"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Duas gravações do índice ao mesmo tempo não o destroem: no fim, decifra.
    #[test]
    fn duas_gravacoes_do_indice_nao_o_destroem() {
        let _guarda_dados = trava_dados();
        let dir = std::env::temp_dir().join(format!("bruma-2grav-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // O `trava_dados` acima garante que nenhum outro teste mexe nesta variável enquanto este corre.
        unsafe { std::env::set_var("BRUMA_DADOS", &dir) };
        let app = Arc::new(App::arrancar().expect("arrancar"));
        // Encher com tamanhos diferentes para o hibrido, se acontecesse, não decifrar.
        for i in 0..20 {
            app.amigos.lock().unwrap().push(Amigo {
                chave: format!("{i:064x}"),
                nome: "x".repeat(i * 3),
                desde_ms: 0,
                verificado: false,
            });
        }

        // Dois threads a gravar em concorrência, um a variar o tamanho do índice. Mede-se o
        // número de gravações que FALHARAM — que, com um temporário partilhado, eram os
        // «sharing violation» do Windows, engolidos em silêncio pelo `let _ =` do chamador.
        // Com o nome único do temporário e o mutex, tem de ser ZERO.
        use std::sync::atomic::{AtomicUsize, Ordering as O};
        let erros = Arc::new(AtomicUsize::new(0));

        let a = Arc::clone(&app);
        let ea = Arc::clone(&erros);
        let t1 = std::thread::spawn(move || {
            for i in 0..400 {
                {
                    let mut am = a.amigos.lock().unwrap();
                    if i % 2 == 0 {
                        am.push(Amigo {
                            chave: format!("{i:064x}"),
                            nome: "z".repeat(i % 50),
                            desde_ms: 0,
                            verificado: false,
                        });
                    } else {
                        am.pop();
                    }
                }
                if a.gravar_indice().is_err() {
                    ea.fetch_add(1, O::Relaxed);
                }
            }
        });
        let b = Arc::clone(&app);
        let eb = Arc::clone(&erros);
        let t2 = std::thread::spawn(move || {
            for _ in 0..400 {
                if b.gravar_indice().is_err() {
                    eb.fetch_add(1, O::Relaxed);
                }
            }
        });
        t1.join().unwrap();
        t2.join().unwrap();

        assert_eq!(
            erros.load(O::Relaxed),
            0,
            "nenhuma gravação concorrente pode falhar"
        );
        // E o índice tem de continuar a decifrar — um híbrido daria erro aqui.
        ler_indice(&dir, &app.semente).expect("o índice tinha de decifrar depois da corrida");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A contagem do que falta ler, sem máquinas nenhumas.
    ///
    /// Está aqui e não no teste de par porque é uma DECISÃO, não um desenho: quem conta,
    /// a partir de quando, e o que não conta. Um teste que precisa de duas instâncias
    /// ligadas mede a orquestração e só de passagem a decisão — e quando falha não diz qual
    /// das duas se partiu.
    /// O por-ler no relógio lógico (#198): uma mensagem escrita depois de o relógio do outro
    /// ser corrigido tem o carimbo cru ATRÁS da marca e o instante à frente — com o carimbo
    /// cru ficava escondida. E o veneno (um carimbo dias à frente) continua a não contar sem
    /// calar o que vem depois dele.
    /// O `provados` é PREGUIÇOSO e certo (#99): construir um servidor não decifra nada; a
    /// primeira pergunta custa uma passagem e responde com toda a gente; a segunda não
    /// custa; e o que se escreve ou junta ANTES da primeira pergunta entra na mesma.
    #[test]
    fn provados_e_preguicoso_e_certo() {
        let dir = std::env::temp_dir().join(format!("bruma-provados-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let gente: Vec<crypto::Identity> = (1u8..=3)
            .map(|k| crypto::Identity::from_seed(&[k; 32]))
            .collect();
        let chave = [9u8; 32];
        let hex = |q: &crypto::Identity| HEXLOWER.encode(q.signing.verifying_key().as_bytes());
        let selada = |texto: &str| {
            let carga = Carga::Mensagem {
                canal: "geral".into(),
                texto: texto.into(),
            };
            crypto::seal(&chave, &serde_json::to_vec(&carga).unwrap()).unwrap()
        };
        let t = agora_ms();
        let mut log = blog::Log::load(dir.join("s.json")).unwrap();
        for (i, quem) in gente.iter().enumerate() {
            let (nonce, ct) = selada(&format!("olá {i}"));
            log.append_local(&quem.signing, nonce, ct, t + i as u64)
                .unwrap();
        }
        let dec = || DECIFRAGENS_NA_THREAD.with(|c| c.get());

        let d0 = dec();
        let mut srv = Servidor::novo("cc".repeat(16), chave, log, vec![], None, None);
        assert_eq!(dec() - d0, 0, "construir não decifra");

        // Escrever e juntar ANTES da primeira pergunta: não custa, e não se perde.
        let quarto = crypto::Identity::from_seed(&[4u8; 32]);
        let mut outro_log = blog::Log::load(dir.join("q.json")).unwrap();
        let (nonce, ct) = selada("juntada");
        outro_log
            .append_local(&quarto.signing, nonce, ct, t + 10)
            .unwrap();
        srv.merge_contado(outro_log.ordered()).unwrap();
        srv.escrever(
            &gente[0].signing,
            &Carga::Mensagem {
                canal: "geral".into(),
                texto: "antes".into(),
            },
        )
        .unwrap();
        assert_eq!(
            dec() - d0,
            0,
            "escrever e juntar antes da primeira pergunta não decifram"
        );

        let d1 = dec();
        let provados = srv.autores_provados().clone();
        assert_eq!(dec() - d1, 1, "a primeira pergunta custa uma passagem");
        for q in &gente {
            assert!(provados.contains(&hex(q)), "os três autores do log");
        }
        assert!(
            provados.contains(&hex(&quarto)),
            "o juntado antes da pergunta também"
        );
        let d2 = dec();
        let _ = srv.autores_provados();
        assert_eq!(dec() - d2, 0, "a segunda pergunta não custa");

        // Depois de inicializada, é o `escrever`/`merge_contado` que a mantém.
        let quinto = crypto::Identity::from_seed(&[5u8; 32]);
        srv.escrever(
            &quinto.signing,
            &Carga::Mensagem {
                canal: "geral".into(),
                texto: "depois".into(),
            },
        )
        .unwrap();
        let d3 = dec();
        assert!(
            srv.autores_provados().contains(&hex(&quinto)),
            "quem escreve depois entra"
        );
        assert_eq!(dec() - d3, 0, "sem passagem nova");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn o_por_ler_no_relogio_logico() {
        let dir = std::env::temp_dir().join(format!("bruma-instante-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let eu = crypto::Identity::from_seed(&[1u8; 32]);
        let outro = crypto::Identity::from_seed(&[2u8; 32]);
        let minha = HEXLOWER.encode(eu.signing.verifying_key().as_bytes());
        let log = blog::Log::load(dir.join("s.json")).unwrap();
        let mut srv = Servidor::novo("bb".repeat(16), [7u8; 32], log, vec![], None, None);
        let t = agora_ms();
        fn escreve_em(srv: &mut Servidor, quem: &crypto::Identity, ts: u64, texto: &str) {
            let carga = Carga::Mensagem {
                canal: "geral".into(),
                texto: texto.into(),
            };
            let claro = serde_json::to_vec(&carga).unwrap();
            let (nonce, ct) = crypto::seal(&srv.chave, &claro).unwrap();
            srv.log.append_local(&quem.signing, nonce, ct, ts).unwrap();
        }
        let contaveis: std::collections::BTreeSet<String> =
            ["geral".to_string()].into_iter().collect();

        // O outro escreve com o relógio 20 s adiantado, e eu leio.
        escreve_em(&mut srv, &outro, t + 20_000, "adiantado");
        let marca = srv.ultima_mensagem("geral", &minha);
        assert_eq!(marca, (t + 20_000) as i64);
        let mut lido = BTreeMap::new();
        lido.insert(App::chave_de_leitura(&srv.id, "geral"), marca);
        assert!(!srv
            .nao_lidos(&minha, &lido, &contaveis)
            .contains_key("geral"));

        // O NTP corrige-o e ele escreve outra vez, agora com o relógio certo.
        escreve_em(&mut srv, &outro, t + 1_000, "corrigido");
        let c = srv.nao_lidos(&minha, &lido, &contaveis);
        assert_eq!(
            c.get("geral").map(|(n, _)| *n),
            Some(1),
            "a mensagem escrita depois da correcção do relógio tem de contar"
        );
        assert_eq!(
            srv.ultima_mensagem("geral", &minha),
            (t + 20_001) as i64,
            "a marca é o instante, não o carimbo"
        );

        // O veneno não conta, e a normal a seguir conta — e lida, nada fica por ler.
        escreve_em(&mut srv, &outro, t + 2 * 86_400_000, "veneno");
        escreve_em(&mut srv, &outro, t + 2_000, "normal");
        let c = srv.nao_lidos(&minha, &lido, &contaveis);
        assert_eq!(
            c.get("geral").map(|(n, _)| *n),
            Some(2),
            "a corrigida e a normal; o veneno não"
        );
        let marca2 = srv.ultima_mensagem("geral", &minha);
        let mut lido2 = BTreeMap::new();
        lido2.insert(App::chave_de_leitura(&srv.id, "geral"), marca2);
        assert!(
            !srv.nao_lidos(&minha, &lido2, &contaveis)
                .contains_key("geral"),
            "lida até à marca, nada fica por ler"
        );

        // E abrir o canal é UMA passagem, sem um único hash recalculado (#90, #154).
        let dec = DECIFRAGENS_NA_THREAD.with(|c| c.get());
        let hashes = blog::HASHES_NA_THREAD.with(|c| c.get());
        let (msgs, ultima) = srv.abrir_canal("geral", &minha);
        assert_eq!(msgs.len(), 4);
        assert_eq!(ultima, marca2);
        assert_eq!(
            DECIFRAGENS_NA_THREAD.with(|c| c.get()) - dec,
            1,
            "uma passagem"
        );
        assert_eq!(
            blog::HASHES_NA_THREAD.with(|c| c.get()) - hashes,
            0,
            "nenhum hash por leitura"
        );
        assert!(msgs.iter().all(|m| m.instante >= m.ts_ms));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn o_que_falta_ler() {
        let dir = std::env::temp_dir().join(format!("bruma-lidos-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let eu = crypto::Identity::from_seed(&[1u8; 32]);
        let outro = crypto::Identity::from_seed(&[2u8; 32]);
        let minha = HEXLOWER.encode(eu.signing.verifying_key().as_bytes());

        let log = blog::Log::load(dir.join("s.json")).unwrap();
        let mut srv = Servidor::novo("aa".repeat(16), [7u8; 32], log, vec![], None, None);

        // Uma função e não um closure: um closure que captura `srv` mutavelmente prende-o
        // durante todo o teste, e as leituras a seguir deixam de compilar.
        fn escreve(srv: &mut Servidor, quem: &crypto::Identity, canal: &str, texto: &str) -> i64 {
            srv.escrever(
                &quem.signing,
                &Carga::Mensagem {
                    canal: canal.into(),
                    texto: texto.into(),
                },
            )
            .unwrap()
            .ts_ms as i64
        }

        escreve(&mut srv, &outro, "geral", "olá");
        escreve(&mut srv, &outro, "geral", "estás aí?");
        let ate = escreve(&mut srv, &outro, "outro", "noutro canal");
        escreve(&mut srv, &eu, "geral", "estou");

        // Os canais que existem. Um canal fora desta lista não pode ser aberto, portanto
        // uma bolha nele nunca se apagava.
        let contaveis: std::collections::BTreeSet<String> =
            ["geral".to_string(), "outro".to_string()]
                .into_iter()
                .collect();

        // Nada lido ainda: as duas dele no geral, a dele no outro, e a MINHA não conta.
        let vazio = BTreeMap::new();
        let c = srv.nao_lidos(&minha, &vazio, &contaveis);
        assert_eq!(c.get("geral").map(|(n, _)| *n), Some(2), "as dele no geral");
        assert_eq!(c.get("outro").map(|(n, _)| *n), Some(1), "a dele no outro");
        assert_eq!(c.len(), 2, "a minha não podia contar");

        // Lido até à última do outro canal: o geral fica limpo, o outro também.
        let mut lido = BTreeMap::new();
        lido.insert(App::chave_de_leitura(&srv.id, "geral"), ate);
        let c = srv.nao_lidos(&minha, &lido, &contaveis);
        assert_eq!(c.get("geral"), None, "o geral tinha de ficar limpo");
        assert_eq!(
            c.get("outro").map(|(n, _)| *n),
            Some(1),
            "o outro não foi lido"
        );

        // Uma nova dele DEPOIS de eu ter lido volta a contar.
        escreve(&mut srv, &outro, "geral", "voltei");
        let c = srv.nao_lidos(&minha, &lido, &contaveis);
        assert_eq!(
            c.get("geral").map(|(n, _)| *n),
            Some(1),
            "a nova tinha de contar"
        );

        // E o `ultima_mensagem` tem de dar a mais recente, senão marcar como lido deixava
        // sempre uma por ler e o contador nunca chegava a zero.
        assert!(srv.ultima_mensagem("geral", &minha) > ate);
        assert_eq!(srv.ultima_mensagem("nao-existe", &minha), 0);

        // UM CANAL QUE NÃO SE PODE ABRIR NÃO CONTA.
        //
        // Três maneiras de ficar com um vermelho preso para sempre: o chat da sala escreve
        // no canal de VOZ, um canal apagado leva a única forma de o abrir, e o id do canal
        // numa carga não é verificado contra nada — qualquer membro inventa um.
        escreve(&mut srv, &outro, "canal-inventado", "não me podes abrir");
        let c = srv.nao_lidos(&minha, &vazio, &contaveis);
        assert_eq!(
            c.get("canal-inventado"),
            None,
            "um canal que não existe não conta"
        );
        // O `ultima_mensagem` DEVOLVE o carimbo dessa entrada, e está certo assim: o guarda
        // é o `contaveis` do `nao_lidos`, e o `marcar_lido` só é chamado com canais que a
        // interface mostra. Pôr aqui um segundo guarda seria a mesma regra em dois sítios —
        // e duas cópias de uma regra são uma regra que um dia deixa de ser a mesma.
        assert!(srv.ultima_mensagem("canal-inventado", &minha) > 0);

        // O RELÓGIO É DE QUEM ESCREVE, E NINGUÉM PROMETEU QUE É HONESTO.
        //
        // Sem tecto, uma mensagem com o ano 9999 marcava o canal como lido até esse futuro:
        // tudo o que viesse depois nascia já lido, em silêncio e para sempre.
        // Forja-se com o `append_local` directamente porque o `escrever` usa o relógio
        // desta máquina: um teste que não consegue mentir na hora não põe esta defesa à
        // prova.
        let mil_anos = agora_ms() + 1000u64 * 60 * 60 * 24 * 365 * 1000;
        let forjada = {
            let carga = Carga::Mensagem {
                canal: "geral".into(),
                texto: "sou do futuro".into(),
            };
            let claro = serde_json::to_vec(&carga).unwrap();
            let (nonce, ct) = crypto::seal(&srv.chave, &claro).unwrap();
            let mut solto = blog::Log::load(dir.join("forja.json")).unwrap();
            solto
                .append_local(&outro.signing, nonce, ct, mil_anos)
                .unwrap()
        };
        assert_eq!(
            srv.merge_contado(vec![forjada]).unwrap(),
            (1, 0),
            "tinha de entrar, e sem gastar orçamento: decifra"
        );

        let marca = srv.ultima_mensagem("geral", &minha);
        assert!(
            marca <= agora_ms() as i64 + 5_000,
            "a marca foi para o futuro: {marca}"
        );
        assert_eq!(
            srv.nao_lidos(&minha, &vazio, &contaveis)
                .get("geral")
                .map(|(n, _)| *n),
            Some(3),
            "a forjada nao pode contar: sao as 3 legitimas dele"
        );

        // E o efeito que interessa: marcar como lido agora não pode calar o que vier a
        // seguir. Sem o tecto, este contador ficaria a zero para sempre.
        let mut lido2 = BTreeMap::new();
        lido2.insert(App::chave_de_leitura(&srv.id, "geral"), marca);
        escreve(&mut srv, &outro, "geral", "e eu venho depois");
        assert_eq!(
            srv.nao_lidos(&minha, &lido2, &contaveis)
                .get("geral")
                .map(|(n, _)| *n),
            Some(1),
            "a mensagem legítima a seguir tinha de contar"
        );

        // E as MINHAS não fazem a marca avançar: sem isto, escrever uma mensagem reescrevia
        // o índice inteiro no disco, e cada escrita dessas é uma janela de perda.
        let antes_de_eu_falar = srv.ultima_mensagem("geral", &minha);
        escreve(&mut srv, &eu, "geral", "sou eu a falar");
        assert_eq!(
            srv.ultima_mensagem("geral", &minha),
            antes_de_eu_falar,
            "a minha mensagem não pode mexer na marca de leitura"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Uma entrada que DECIFRA mas está mal assinada não dá direitos a ninguém.
    ///
    /// Qualquer membro da sala consegue fazer uma entrada que decifra — tem a chave. Se lhe
    /// puser no `author` a chave de um terceiro e assinar com lixo, o `merge` rejeita a
    /// entrada; a questão é se o terceiro fica na lista dos provados na mesma. Ficava.
    ///
    /// O teste tem de provar as DUAS metades: a forjada sai **e** a legítima entra. Apertar
    /// isto de mais deixaria de aprender pares verdadeiros, e o sintoma seria «às vezes não
    /// o vejo na chamada» — muito pior de encontrar do que este.
    #[test]
    fn assinatura_ma_nao_da_direitos() {
        let dir = std::env::temp_dir().join(format!("bruma-assin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let membro = crypto::Identity::from_seed(&[9u8; 32]);
        let chave_da_sala = [7u8; 32];
        let terceiro = "cc".repeat(32);

        let log = blog::Log::load(dir.join("s.json")).unwrap();
        let mut srv = Servidor::novo("dd".repeat(16), chave_da_sala, log, vec![], None, None);

        // Uma entrada legítima do membro, para o caminho bom ficar provado no mesmo teste.
        let boa = {
            let carga = Carga::Mensagem {
                canal: "geral".into(),
                texto: "sou membro".into(),
            };
            let claro = serde_json::to_vec(&carga).unwrap();
            let (nonce, ct) = crypto::seal(&chave_da_sala, &claro).unwrap();
            let mut solto = blog::Log::load(dir.join("bom.json")).unwrap();
            solto
                .append_local(&membro.signing, nonce, ct, agora_ms())
                .unwrap()
        };

        // E a forjada: decifra na mesma (a chave da sala é a mesma), mas o `author` é de um
        // terceiro e a assinatura é lixo.
        let mut forjada = boa.clone();
        forjada.author = terceiro.clone();
        forjada.sig = "00".repeat(64);

        // E a entrada forjada gasta orçamento (#85): é lixo, e é isso que o porteiro conta.
        let (entraram, recusadas) = srv.merge_contado(vec![boa, forjada]).unwrap();
        assert_eq!(recusadas, 1, "a forjada tem de contar como lixo");

        let provados = srv.autores_provados();
        let chave_do_membro = HEXLOWER.encode(membro.signing.verifying_key().as_bytes());
        assert!(
            provados.contains(&chave_do_membro),
            "o membro legítimo tinha de ficar provado"
        );
        assert!(
            !provados.contains(&terceiro),
            "o terceiro NÃO pode ficar provado: a entrada dele nem sequer entrou no log"
        );
        assert_eq!(entraram, 1, "só a boa podia entrar");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Marcar como lido só AVANÇA.
    ///
    /// Se recuasse, uma marcação atrasada (a app aberta duas vezes, um evento fora de ordem)
    /// ressuscitava mensagens já lidas — e o sintoma seria «o não lido volta sozinho», que
    /// não se liga a esta linha de código de maneira nenhuma.
    #[test]
    fn marcar_lido_nunca_recua() {
        let _guarda_dados = trava_dados();
        let dir = std::env::temp_dir().join(format!("bruma-recua-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // O `trava_dados` acima garante que nenhum outro teste mexe nesta variável enquanto este corre.
        unsafe { std::env::set_var("BRUMA_DADOS", &dir) };
        let app = App::arrancar().expect("arrancar");

        assert!(app.marcar_lido("s", "c", 100), "a primeira marca");
        assert_eq!(app.lido_ate("s", "c"), 100);
        assert!(!app.marcar_lido("s", "c", 50), "50 é para trás: não muda");
        assert_eq!(app.lido_ate("s", "c"), 100, "e não pode ter recuado");
        assert!(app.marcar_lido("s", "c", 150), "150 é para a frente");
        assert_eq!(app.lido_ate("s", "c"), 150);
        assert_eq!(app.lido_ate("s", "outro"), 0, "outro canal, outro contador");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Duas ordens de lock contrárias param os dois threads, para sempre, sem um erro.
    ///
    /// Este é o género de avaria que não aparece em nenhum registo: a app fica quieta, e a
    /// única prova é o tempo a passar.
    ///
    /// # Porque é que NÃO tem um prazo fixo
    ///
    /// A primeira versão dava 30 segundos para 4000 voltas acabarem. Passava aqui e falhou
    /// no runner — e não por estar presa: por o `gravar_indice` ter passado a fazer
    /// `sync_data`, que custa **18 vezes mais** por escrita. Um prazo fixo mede a velocidade
    /// da máquina e chama-lhe deadlock; o CI ficou vermelho a apontar para o sítio errado.
    ///
    /// Um deadlock é **ausência de progresso**, e é isso que se mede: cada volta incrementa
    /// um contador, e o que faz o teste falhar é o contador deixar de mexer. Numa máquina
    /// lenta demora mais e passa na mesma; presa, falha em segundos, seja qual for a
    /// máquina.
    #[test]
    fn duas_ordens_de_lock_nao_se_prendem() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let _guarda_dados = trava_dados();
        let dir = std::env::temp_dir().join(format!("bruma-lock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // O `trava_dados` acima garante que nenhum outro teste mexe nesta variável enquanto este corre.
        unsafe { std::env::set_var("BRUMA_DADOS", &dir) };

        let app = Arc::new(App::arrancar().expect("arrancar"));
        *app.quem_escreve.lock().unwrap() = QuemEscreve::Salas;

        let voltas = 600;
        let feitas = Arc::new([AtomicUsize::new(0), AtomicUsize::new(0)]);

        let a = Arc::clone(&app);
        let ca = Arc::clone(&feitas);
        std::thread::spawn(move || {
            for _ in 0..voltas {
                let _ = a.pode_escrever_me("aa");
                ca[0].fetch_add(1, Ordering::Relaxed);
            }
        });

        let b = Arc::clone(&app);
        let cb = Arc::clone(&feitas);
        std::thread::spawn(move || {
            for _ in 0..voltas {
                let _ = b.gravar_indice();
                cb[1].fetch_add(1, Ordering::Relaxed);
            }
        });

        // Enquanto os contadores mexerem, espera-se. Quando pararem sem terem acabado, é
        // porque se prenderam.
        let paragem = std::time::Duration::from_secs(20);
        let mut ultimo = (0, 0);
        let mut parado_desde = std::time::Instant::now();
        loop {
            std::thread::sleep(std::time::Duration::from_millis(200));
            let agora = (
                feitas[0].load(Ordering::Relaxed),
                feitas[1].load(Ordering::Relaxed),
            );
            if agora.0 >= voltas && agora.1 >= voltas {
                break;
            }
            if agora != ultimo {
                ultimo = agora;
                parado_desde = std::time::Instant::now();
                continue;
            }
            assert!(
                parado_desde.elapsed() < paragem,
                "prendeu-se: {} e {} voltas de {voltas}, sem mexer há {paragem:?} —                  ordens de lock contrárias",
                agora.0,
                agora.1
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
