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
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Indice {
    #[serde(default)]
    pub servidores: Vec<ServidorGuardado>,
    #[serde(default)]
    pub nome: String,
}

pub struct Servidor {
    pub id: String,
    pub chave: [u8; 32],
    pub log: blog::Log,
    pub peers: Vec<String>,
}

impl Servidor {
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
        self.log.append_local(signing, nonce, ct, agora_ms())
    }
}

pub struct App {
    pub ident: crypto::Identity,
    pub semente: [u8; 32],
    pub nome: Mutex<String>,
    pub servidores: Mutex<BTreeMap<String, Servidor>>,
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
            servidores.insert(
                s.id.clone(),
                Servidor {
                    id: s.id.clone(),
                    chave,
                    log,
                    peers: s.peers.clone(),
                },
            );
        }

        let app = App {
            ident,
            semente,
            nome: Mutex::new(indice.nome),
            servidores: Mutex::new(servidores),
        };

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

    pub fn gravar_indice(&self) -> Result<()> {
        let servidores = self.servidores.lock().unwrap();
        let indice = Indice {
            nome: self.nome.lock().unwrap().clone(),
            servidores: servidores
                .values()
                .map(|s| ServidorGuardado {
                    id: s.id.clone(),
                    chave: HEXLOWER.encode(&s.chave),
                    peers: s.peers.clone(),
                })
                .collect(),
        };
        // CIFRADO, sempre. O que aqui está são as chaves de todos os servidores.
        let claro = serde_json::to_vec(&indice)?;
        let (nonce, dados) = crypto::seal(&crypto::chave_do_indice(&self.semente), &claro)?;
        let cofre = Cofre {
            v: 1,
            nonce: HEXLOWER.encode(&nonce),
            dados: HEXLOWER.encode(&dados),
        };
        std::fs::write(
            raiz().join("indice.json"),
            serde_json::to_string_pretty(&cofre)?,
        )?;
        Ok(())
    }
}

/// Renomeia um ficheiro que não se conseguiu ler, para não voltar a matar o arranque.
///
/// Guarda-se em vez de se apagar: pode ser a única cópia de uma conversa.
fn pos_de_lado(p: &std::path::Path) {
    let destino = p.with_extension("json.estragado");
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
        if raw.len() == 32 {
            let mut s = [0u8; 32];
            s.copy_from_slice(&raw);
            return Ok(s);
        }
        bail!("{} existe mas não tem 32 bytes", p.display());
    }
    let mut s = [0u8; 32];
    getrandom::getrandom(&mut s).map_err(|e| anyhow!("rng: {e}"))?;
    std::fs::write(p, s)?;
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

pub fn caminho_do_log(id: &str) -> PathBuf {
    raiz().join("servidores").join(format!("{id}.json"))
}
