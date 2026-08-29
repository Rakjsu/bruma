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
    if let Ok(escolhida) = std::env::var("BRUMA_DADOS") {
        return PathBuf::from(escolhida);
    }
    let ao_lado = PathBuf::from("dados");
    if ao_lado.join("identidade.key").exists() {
        return ao_lado;
    }
    pasta_do_sistema().unwrap_or(ao_lado)
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
    /// Cache de `autores_provados`, refeita a cada `merge`/`escrever`.
    ///
    /// Sem isto, o `aprender_dos_logs` decifrava TODOS os logs — com o lock global preso —
    /// a cada ligação de qualquer estranho. Era um caminho de negação de serviço aberto por
    /// mim ao fechar outro.
    provados: std::collections::BTreeSet<String>,
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

impl Servidor {
    /// O único caminho para construir um `Servidor`.
    ///
    /// A cache `provados` é privada de propósito: se fosse pública, alguém a preencheria à
    /// mão e o «provado» deixava de querer dizer alguma coisa. Aqui ela é sempre derivada do
    /// log, com uma passagem de decifragem.
    pub fn novo(
        id: String,
        chave: [u8; 32],
        log: blog::Log,
        peers: Vec<String>,
        convidou: Option<String>,
        com: Option<String>,
    ) -> Self {
        let mut s = Servidor {
            id,
            chave,
            log,
            peers,
            convidou,
            com,
            provados: Default::default(),
        };
        s.recontar_provados();
        s
    }

    /// Decifra o que conseguir e devolve as entradas prontas a aplicar, pela ordem do log.
    ///
    /// O que não decifrar é **ignorado em silêncio, não rejeitado**: numa app onde a chave
    /// pode rodar, ter entradas que já não se conseguem ler é normal, não é corrupção.
    pub fn aplicaveis(&self) -> (Vec<Aplicavel>, Vec<String>) {
        let mut saida = Vec::new();
        let mut ids = Vec::new();
        for e in self.log.ordered() {
            let (Ok(nonce), Ok(ct), Ok(id)) = (
                hex24(&e.nonce),
                HEXLOWER.decode(e.ciphertext.as_bytes()),
                e.hash_hex(),
            ) else {
                continue;
            };
            let Ok(claro) = crypto::open(&self.chave, &nonce, &ct) else {
                continue;
            };
            let Ok(carga) = serde_json::from_slice::<Carga>(&claro) else {
                continue;
            };
            saida.push(Aplicavel {
                autor: e.author.clone(),
                ts_ms: e.ts_ms,
                carga,
            });
            ids.push(id);
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
        &self.provados
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
        let limite = agora_ms() as i64 + DESVIO_TOLERADO_MS;
        self.aplicaveis()
            .0
            .iter()
            .filter_map(|a| match &a.carga {
                Carga::Mensagem { canal: c, .. } if c == canal && a.autor != eu => {
                    let ts = a.ts_ms.min(i64::MAX as u64) as i64;
                    (ts <= limite).then_some(ts)
                }
                _ => None,
            })
            .max()
            .unwrap_or(0)
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
            if ts > ate {
                let e = fora.entry(canal.clone()).or_insert((0, 0));
                e.0 += 1;
                e.1 = e.1.max(ts);
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
        let (nonce, ct) = crypto::seal(&self.chave, &claro)?;
        let e = self.log.append_local(signing, nonce, ct, agora_ms())?;
        self.provados.insert(e.author.clone());
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
    pub fn merge_verificado(&mut self, entradas: Vec<blog::Entry>) -> Result<usize> {
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
                let (Ok(nonce), Ok(ct)) =
                    (hex24(&e.nonce), HEXLOWER.decode(e.ciphertext.as_bytes()))
                else {
                    return false;
                };
                crypto::open(&self.chave, &nonce, &ct).is_ok()
            })
            .collect();
        for e in &boas {
            self.provados.insert(e.author.clone());
        }
        self.log.merge(boas)
    }

    /// Refaz a cache de quem provou. Só ao abrir a app, e uma vez por servidor.
    fn recontar_provados(&mut self) {
        self.provados = self.aplicaveis().0.into_iter().map(|a| a.autor).collect();
    }
}

pub struct App {
    pub ident: crypto::Identity,
    pub semente: [u8; 32],
    pub nome: Mutex<String>,
    pub servidores: Mutex<BTreeMap<String, Servidor>>,
    pub prekeys: Mutex<BTreeMap<String, String>>,
    pub amigos: Mutex<Vec<Amigo>>,
    pub bloqueados: Mutex<Vec<String>>,
    pub quem_escreve: Mutex<QuemEscreve>,
    /// Ver [`Indice::lido`].
    pub lido: Mutex<BTreeMap<String, i64>>,
    /// Serializa `gravar_indice` — duas escritas ao mesmo tempo destruíam o índice. Não guarda
    /// nada; é só um portão. É privado de propósito: se alguém o tomasse antes de chamar
    /// `gravar_indice`, prendia.
    escrita_do_indice: Mutex<()>,
}

impl App {
    pub fn arrancar() -> Result<Self> {
        let raiz = raiz();
        std::fs::create_dir_all(raiz.join("servidores"))?;
        let semente = semente_ou_cria(&raiz.join("identidade.key"))?;
        let ident = crypto::Identity::from_seed(&semente);

        let (indice, em_claro) = ler_indice(&raiz, &semente)?;
        let mut servidores = BTreeMap::new();
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
                        "[dados] o servidor {} tem a chave estragada ({e}); fica de fora",
                        s.id
                    );
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
                    // de uma conversa, e quem souber ler JSON ainda a tira de lá. Apagá-lo
                    // seria a app decidir sozinha deitar fora o que não conseguiu abrir.
                    pos_de_lado(&caminho);
                    continue;
                }
            };
            let mut srv = Servidor {
                id: s.id.clone(),
                chave,
                log,
                peers: s.peers.clone(),
                convidou: s.convidou.clone(),
                com: s.com.clone(),
                provados: Default::default(),
            };
            // Uma passagem por servidor, aqui e só aqui. A partir daqui a cache mantém-se
            // sozinha no `merge_verificado` e no `escrever`.
            srv.recontar_provados();
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
            amigos: Mutex::new(indice.amigos),
            bloqueados: Mutex::new(indice.bloqueados),
            quem_escreve: Mutex::new(indice.quem_escreve),
            lido: Mutex::new(indice.lido),
            escrita_do_indice: Mutex::new(()),
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
        if m.get(peer).map(|v| v.as_str()) == Some(x_pub) {
            return Ok(()); // já a tínhamos, e é a mesma
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
        self.servidores.lock().unwrap().insert(id.clone(), {
            let mut srv = Servidor {
                id: id.clone(),
                chave,
                log,
                // O outro é o único par desta conversa, e já é conhecido desde o início:
                // não há aqui o passo de "dar-se a conhecer" que os servidores têm.
                peers: vec![peer.to_string()],
                convidou: None,
                com: Some(peer.to_string()),
                provados: Default::default(),
            };
            srv.recontar_provados();
            srv
        });
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

    pub fn gravar_indice(&self) -> Result<()> {
        // Recolher primeiro, e largar. Isto segurava `servidores` durante a serialização, a
        // cifra e a escrita no disco, enquanto ia pedindo os outros quatro — uma janela larga
        // para outro thread ficar preso do outro lado.
        let guardados: Vec<ServidorGuardado> = {
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
        let indice = Indice {
            nome: self.nome.lock().unwrap().clone(),
            servidores: guardados,
            prekeys: self.prekeys.lock().unwrap().clone(),
            amigos: self.amigos.lock().unwrap().clone(),
            bloqueados: self.bloqueados.lock().unwrap().clone(),
            quem_escreve: *self.quem_escreve.lock().unwrap(),
            lido: self.lido.lock().unwrap().clone(),
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
        escrever_atomico(&raiz().join("indice.json"), &bytes)
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
#[derive(Serialize, Deserialize)]
struct Cofre {
    v: u8,
    nonce: String,
    dados: String,
}

fn ler_indice(raiz: &std::path::Path, semente: &[u8; 32]) -> Result<(Indice, bool)> {
    let p = raiz.join("indice.json");
    if !p.exists() {
        return Ok((Indice::default(), false));
    }
    let bruto = std::fs::read_to_string(&p)?;

    // Cifrado é o formato de hoje.
    if let Ok(cofre) = serde_json::from_str::<Cofre>(&bruto) {
        let nonce = hex24(&cofre.nonce)?;
        let dados = HEXLOWER
            .decode(cofre.dados.as_bytes())
            .map_err(|e| anyhow!("índice ilegível: {e}"))?;
        let claro = crypto::open(&crypto::chave_do_indice(semente), &nonce, &dados)?;
        return Ok((serde_json::from_slice(&claro)?, false));
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
    let mut com_soma = Vec::with_capacity(36);
    com_soma.extend_from_slice(&s);
    com_soma.extend_from_slice(&crypto::soma_de_controlo(&s));
    escrever_atomico(p, &com_soma)?;
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
    use std::sync::Arc;

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
        let dir = std::env::temp_dir().join(format!("bruma-2grav-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // SAFETY: um só teste toca nesta variável, antes de qualquer thread arrancar.
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
            srv.merge_verificado(vec![forjada]).unwrap(),
            1,
            "tinha de entrar"
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

        let entraram = srv.merge_verificado(vec![boa, forjada]).unwrap();

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
        let dir = std::env::temp_dir().join(format!("bruma-recua-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // SAFETY: um só teste toca nesta variável, e antes de qualquer thread arrancar.
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

        let dir = std::env::temp_dir().join(format!("bruma-lock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // SAFETY: um só teste toca nesta variável, e antes de qualquer thread arrancar.
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
