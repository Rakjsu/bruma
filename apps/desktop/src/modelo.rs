//! O modelo do Bruma: servidores, canais, membros e convites.
//!
//! Decisão de fundo: **cada servidor tem UM log**. A carga cifrada de cada entrada é ou uma
//! mensagem ou uma operação de configuração (criar canal, mudar o nome, apresentar-se). Assim
//! o mesmo caminho de sincronização, a mesma criptografia e a mesma ordenação servem para
//! tudo — não há um segundo sistema para a configuração que possa divergir do primeiro.
//!
//! O estado visível é sempre RECONSTRUÍDO a partir do log, nunca guardado à parte. Isso
//! garante que dois membros com as mesmas entradas veem exatamente o mesmo servidor, sem
//! precisarem de falar um com o outro para acertar.

use anyhow::{anyhow, Result};
use data_encoding::BASE32_NOPAD;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub type Id = [u8; 16];

/// Prefixo dos convites. Serve para a pessoa perceber o que lhe foi enviado e para nós
/// recusarmos cedo o que não é nosso.
const PREFIXO_CONVITE: &str = "bruma1";

pub fn novo_id() -> Result<Id> {
    let mut b = [0u8; 16];
    getrandom::getrandom(&mut b).map_err(|e| anyhow!("rng: {e}"))?;
    Ok(b)
}

pub fn id_hex(id: &Id) -> String {
    data_encoding::HEXLOWER.encode(id)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TipoCanal {
    Texto,
    Voz,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Canal {
    pub id: String,
    pub nome: String,
    pub tipo: TipoCanal,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Membro {
    /// Chave pública em hex. É o ID da pessoa; não há nome de utilizador reservado.
    pub chave: String,
    pub nome: String,
    /// Se é o fundador do servidor. O fundador é simplesmente quem escreveu a primeira
    /// operação — não há registo de "dono" para se falsificar.
    pub fundador: bool,
}

/// O que vai dentro da carga cifrada de cada entrada do log.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "t")]
pub enum Carga {
    Mensagem {
        canal: String,
        texto: String,
    },
    NomeDoServidor {
        nome: String,
    },
    CriarCanal {
        id: String,
        nome: String,
        tipo: TipoCanal,
    },
    ApagarCanal {
        id: String,
    },
    /// "Sou eu, e chamo-me assim." A identidade vem do autor da entrada, não daqui —
    /// isto só transporta o nome que a pessoa escolheu mostrar.
    Apresentar {
        nome: String,
    },
    /// Uma carga escrita por uma versão MAIS RECENTE do Bruma.
    ///
    /// Sem isto, uma carga que o serde não reconhece caía num `continue` no `aplicaveis`:
    /// desaparecia da vista, sem erro, sem sinal. No dia em que a v0.18 mandasse uma reacção
    /// ou um anexo, a v0.17 via um BURACO onde estava uma mensagem — e a app ficava a guardar
    /// no disco aquilo que se recusava a mostrar, sem o dizer.
    ///
    /// Não é `#[serde(other)]` (que num enum etiquetado só admite variante sem dados) mas uma
    /// leitura em dois passos: assim guarda-se o `t` que veio e o `canal`, se houver. É a
    /// diferença entre «há aqui uma coisa que não sei mostrar» no sítio certo e um silêncio.
    Desconhecida {
        /// A etiqueta que a versão nova usou. Chama-se `etiqueta` e não `t` porque o serde
        /// recusa um campo com o mesmo nome da etiqueta do enum — e com razão.
        etiqueta: String,
        /// O canal, quando a carga desconhecida tem um — uma carga futura do género mensagem
        /// terá. Sem canal, não é uma coisa da linha do tempo e não se mostra lá.
        canal: Option<String>,
    },
}

/// O mínimo que se lê de uma carga que não se reconhece: a etiqueta e, se houver, o canal.
#[derive(Deserialize)]
pub struct CargaCrua {
    pub t: String,
    #[serde(default)]
    pub canal: Option<String>,
}

/// Uma entrada já decifrada e pronta a aplicar.
#[derive(Clone, Debug)]
pub struct Aplicavel {
    pub autor: String,
    pub ts_ms: u64,
    pub carga: Carga,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MensagemVista {
    pub id: String,
    pub autor: String,
    pub autor_nome: String,
    pub canal: String,
    pub ts_ms: u64,
    pub texto: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EstadoDoServidor {
    pub nome: String,
    pub canais: Vec<Canal>,
    pub membros: Vec<Membro>,
}

/// Reconstrói o estado visível a partir das entradas, já pela ordem definida pelo log.
///
/// É deliberadamente uma função pura: não lê ficheiros, não fala com a rede, e dá sempre o
/// mesmo resultado para a mesma lista. É isso que torna o estado testável e que garante que
/// dois membros convergem sem negociar nada.
/// O nome mostrável de um autor, ou «desconhecido».
fn nome_de(estado: &EstadoDoServidor, autor: &str) -> String {
    estado
        .membros
        .iter()
        .find(|m| m.chave == autor)
        .map(|m| m.nome.clone())
        .unwrap_or_else(|| "desconhecido".into())
}

pub fn reconstruir(entradas: &[Aplicavel]) -> EstadoDoServidor {
    let mut nome = String::new();
    let mut canais: Vec<Canal> = Vec::new();
    let mut apagados: Vec<String> = Vec::new();
    let mut nomes: BTreeMap<String, String> = BTreeMap::new();
    let mut ordem_de_chegada: Vec<String> = Vec::new();

    for e in entradas {
        if !nomes.contains_key(&e.autor) {
            nomes.insert(e.autor.clone(), String::new());
            ordem_de_chegada.push(e.autor.clone());
        }
        match &e.carga {
            Carga::NomeDoServidor { nome: n } => nome = n.clone(),
            Carga::CriarCanal { id, nome: n, tipo } => {
                if !canais.iter().any(|c| &c.id == id) {
                    canais.push(Canal {
                        id: id.clone(),
                        nome: n.clone(),
                        tipo: *tipo,
                    });
                }
            }
            Carga::ApagarCanal { id } => apagados.push(id.clone()),
            Carga::Apresentar { nome: n } => {
                nomes.insert(e.autor.clone(), n.clone());
            }
            Carga::Mensagem { .. } => {}
            // Uma carga de uma versão mais recente não muda o estado — o que ela faria, esta
            // versão não sabe. O AUTOR conta na mesma (o `nomes` acima já o registou), para a
            // pessoa não sumir da lista de membros só por ter uma versão mais nova.
            Carga::Desconhecida { .. } => {}
        }
    }

    canais.retain(|c| !apagados.contains(&c.id));

    let membros = ordem_de_chegada
        .iter()
        .enumerate()
        .map(|(i, chave)| Membro {
            chave: chave.clone(),
            nome: nomes
                .get(chave)
                .filter(|n| !n.is_empty())
                .cloned()
                // Sem apresentação, mostra-se um pedaço da chave. Não se inventa um nome.
                .unwrap_or_else(|| format!("{}…", &chave[..chave.len().min(6)])),
            fundador: i == 0,
        })
        .collect();

    EstadoDoServidor {
        nome,
        canais,
        membros,
    }
}

/// Extrai as mensagens de um canal, já ordenadas como vieram.
/// O canal de uma conversa privada.
///
/// Uma conversa não tem canais — não há onde os criar nem quem os crie. Mas o caminho das
/// mensagens é o mesmo dos servidores e pede um canal, por isso usa-se um id fixo. Fixo e
/// não sorteado, para os dois lados chegarem lá sem combinar nada, como o id da conversa.
pub const CANAL_DA_CONVERSA: &str = "conversa";

pub fn mensagens_do_canal(
    entradas: &[Aplicavel],
    ids: &[String],
    canal: &str,
    estado: &EstadoDoServidor,
) -> Vec<MensagemVista> {
    entradas
        .iter()
        .zip(ids.iter())
        .filter_map(|(e, id)| match &e.carga {
            Carga::Mensagem { canal: c, texto } if c == canal => Some(MensagemVista {
                id: id.clone(),
                autor: e.autor.clone(),
                autor_nome: nome_de(estado, &e.autor),
                canal: c.clone(),
                ts_ms: e.ts_ms,
                texto: texto.clone(),
            }),
            // Uma carga de uma versão mais recente, NESTE canal: mostra-se um marcador com o
            // autor e a hora reais, em vez de um buraco. A pessoa vê que ali houve alguma
            // coisa, e porque não a vê.
            Carga::Desconhecida {
                canal: Some(c), ..
            } if c == canal => Some(MensagemVista {
                id: id.clone(),
                autor: e.autor.clone(),
                autor_nome: nome_de(estado, &e.autor),
                canal: c.clone(),
                ts_ms: e.ts_ms,
                texto: "(uma mensagem escrita por uma versão mais recente do Bruma —                         actualiza para a veres)"
                    .into(),
            }),
            _ => None,
        })
        .collect()
}

/// O que um convite carrega.
///
/// ⚠️ **Um convite contém a chave do servidor.** Quem o tiver consegue ler tudo o que lá for
/// escrito a partir do momento em que entra. Não é um endereço público: é um segredo, e a
/// interface tem de o dizer com essas palavras.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Convite {
    pub servidor: String,
    pub nome: String,
    pub chave: String,
    /// O `EndpointId` de quem convidou — é por aí que se entra na rede do servidor.
    pub anfitriao: String,
}

impl Convite {
    /// Codifica em base32 minúsculo sem padding.
    ///
    /// Base32 e não hex nem base64 por uma razão prática aprendida no teste do spike 1: o
    /// código vai ser copiado de aplicações de mensagens e ditado ao telefone. Base32 não
    /// tem maiúsculas para trocar nem os caracteres que se confundem em base64.
    pub fn codificar(&self) -> Result<String> {
        let bruto = serde_json::to_vec(self)?;
        Ok(format!(
            "{PREFIXO_CONVITE}{}",
            BASE32_NOPAD.encode(&bruto).to_lowercase()
        ))
    }

    pub fn descodificar(s: &str) -> Result<Self> {
        // Tolerante de propósito: espaços, quebras de linha e maiúsculas aparecem sempre que
        // alguém copia de uma app de mensagens.
        let limpo: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        let limpo = limpo.trim_start_matches("bruma://");
        let corpo = limpo
            .strip_prefix(PREFIXO_CONVITE)
            .ok_or_else(|| anyhow!("isto não parece um convite do Bruma"))?;
        let bruto = BASE32_NOPAD
            .decode(corpo.to_uppercase().as_bytes())
            .map_err(|_| anyhow!("o convite está incompleto ou foi copiado a meio"))?;
        serde_json::from_slice(&bruto).map_err(|_| anyhow!("o convite está corrompido"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ap(autor: &str, ts: u64, carga: Carga) -> Aplicavel {
        Aplicavel {
            autor: autor.into(),
            ts_ms: ts,
            carga,
        }
    }

    /// Uma carga de uma versão MAIS RECENTE aparece na linha do tempo, em vez de sumir.
    ///
    /// Sem isto, a v0.17 via um buraco onde a v0.18 escreveu — e guardava a entrada no disco
    /// na mesma, sem o dizer. O marcador leva o autor e a hora reais: a pessoa vê que ali
    /// houve alguma coisa, e porquê não a vê.
    #[test]
    fn carga_de_versao_mais_recente_aparece_como_marcador() {
        let entradas = vec![
            ap(
                "aa",
                1,
                Carga::Apresentar {
                    nome: "Rakjsu".into(),
                },
            ),
            ap(
                "aa",
                2,
                Carga::Mensagem {
                    canal: "geral".into(),
                    texto: "olá".into(),
                },
            ),
            // O que a v0.18 mandaria: uma carga que esta versão não conhece, num canal.
            ap(
                "aa",
                3,
                Carga::Desconhecida {
                    etiqueta: "Reaccao".into(),
                    canal: Some("geral".into()),
                },
            ),
            // E uma sem canal — não é da linha do tempo, não se mostra lá.
            ap(
                "aa",
                4,
                Carga::Desconhecida {
                    etiqueta: "SejaOQueFor".into(),
                    canal: None,
                },
            ),
        ];
        let ids: Vec<String> = (0..entradas.len()).map(|i| format!("id{i}")).collect();
        let estado = reconstruir(&entradas);
        let msgs = mensagens_do_canal(&entradas, &ids, "geral", &estado);

        assert_eq!(
            msgs.len(),
            2,
            "a mensagem e o marcador, e não a carga sem canal"
        );
        assert_eq!(msgs[1].autor, "aa", "o marcador leva o autor real");
        assert_eq!(msgs[1].ts_ms, 3, "e a hora real");
        assert_eq!(msgs[1].autor_nome, "Rakjsu", "e o nome, não «desconhecido»");
        assert!(
            msgs[1].texto.contains("versão mais recente"),
            "e diz porque não se vê: {}",
            msgs[1].texto
        );
    }

    /// A leitura em dois passos: uma carga com um `t` novo lê-se como desconhecida.
    #[test]
    fn carga_com_etiqueta_nova_le_se_como_desconhecida() {
        // O que a v0.18 escreveria dentro do cifrado.
        let futuro = br#"{"t":"Anexo","canal":"geral","ficheiro":"foto.png","bytes":123}"#;
        // A `Carga` normal não a conhece...
        assert!(serde_json::from_slice::<Carga>(futuro).is_err());
        // ...mas o segundo passo tira-lhe a etiqueta e o canal.
        let crua: CargaCrua = serde_json::from_slice(futuro).expect("o segundo passo tem de ler");
        assert_eq!(crua.t, "Anexo");
        assert_eq!(crua.canal.as_deref(), Some("geral"));

        // E o que se conhece continua a ser conhecido — um catch-all mal posto faria TUDO
        // cair no desconhecido, e a app ficava muda sem um erro.
        let msg: Carga =
            serde_json::from_slice(br#"{"t":"Mensagem","canal":"geral","texto":"ola"}"#).unwrap();
        assert!(
            matches!(msg, Carga::Mensagem { .. }),
            "a Mensagem tem de continuar a ler-se"
        );
    }

    #[test]
    fn o_fundador_e_quem_escreveu_primeiro() {
        let e = vec![
            ap(
                "aaa",
                1,
                Carga::NomeDoServidor {
                    nome: "Casa".into(),
                },
            ),
            ap("bbb", 2, Carga::Apresentar { nome: "rui".into() }),
        ];
        let s = reconstruir(&e);
        assert_eq!(s.nome, "Casa");
        assert!(s.membros[0].fundador, "o primeiro a escrever e o fundador");
        assert!(!s.membros[1].fundador);
    }

    #[test]
    fn sem_apresentacao_mostra_um_pedaco_da_chave() {
        // Nao se inventa um nome nem se poe "Anonimo": mostra-se o que ha, que e a chave.
        let s = reconstruir(&[ap(
            "abcdef123456",
            1,
            Carga::Mensagem {
                canal: "c".into(),
                texto: "oi".into(),
            },
        )]);
        assert_eq!(s.membros[0].nome, "abcdef…");
    }

    #[test]
    fn criar_e_apagar_canais() {
        let e = vec![
            ap(
                "aaa",
                1,
                Carga::CriarCanal {
                    id: "c1".into(),
                    nome: "geral".into(),
                    tipo: TipoCanal::Texto,
                },
            ),
            ap(
                "aaa",
                2,
                Carga::CriarCanal {
                    id: "c2".into(),
                    nome: "voz".into(),
                    tipo: TipoCanal::Voz,
                },
            ),
            ap("aaa", 3, Carga::ApagarCanal { id: "c1".into() }),
        ];
        let s = reconstruir(&e);
        assert_eq!(s.canais.len(), 1);
        assert_eq!(s.canais[0].nome, "voz");
        assert_eq!(s.canais[0].tipo, TipoCanal::Voz);
    }

    #[test]
    fn criar_o_mesmo_canal_duas_vezes_nao_duplica() {
        // Dois membros podem criar o mesmo canal em concorrencia; o id e que manda.
        let e = vec![
            ap(
                "aaa",
                1,
                Carga::CriarCanal {
                    id: "c1".into(),
                    nome: "geral".into(),
                    tipo: TipoCanal::Texto,
                },
            ),
            ap(
                "bbb",
                2,
                Carga::CriarCanal {
                    id: "c1".into(),
                    nome: "geral".into(),
                    tipo: TipoCanal::Texto,
                },
            ),
        ];
        assert_eq!(reconstruir(&e).canais.len(), 1);
    }

    #[test]
    fn o_ultimo_nome_do_servidor_ganha() {
        let e = vec![
            ap(
                "aaa",
                1,
                Carga::NomeDoServidor {
                    nome: "Antigo".into(),
                },
            ),
            ap(
                "bbb",
                2,
                Carga::NomeDoServidor {
                    nome: "Novo".into(),
                },
            ),
        ];
        assert_eq!(reconstruir(&e).nome, "Novo");
    }

    #[test]
    fn convite_ida_e_volta() {
        let c = Convite {
            servidor: "0123456789abcdef0123456789abcdef".into(),
            nome: "Casa da Névoa".into(),
            chave: "ff".repeat(32),
            anfitriao: "aa".repeat(32),
        };
        let codigo = c.codificar().unwrap();
        assert!(codigo.starts_with("bruma1"));
        assert_eq!(Convite::descodificar(&codigo).unwrap(), c);
    }

    #[test]
    fn convite_sobrevive_a_ser_copiado_de_uma_app_de_mensagens() {
        // O caso real: a app quebra a linha, mete espacos, ou o telemovel poe maiuscula.
        let c = Convite {
            servidor: "00".repeat(16),
            nome: "Casa".into(),
            chave: "11".repeat(32),
            anfitriao: "22".repeat(32),
        };
        let codigo = c.codificar().unwrap();
        let meio = codigo.len() / 2;
        let sujo = format!(" {}\n  {} \r\n", &codigo[..meio], &codigo[meio..]);
        assert_eq!(Convite::descodificar(&sujo).unwrap(), c);
        // E com o esquema de link a frente.
        assert_eq!(
            Convite::descodificar(&format!("bruma://{codigo}")).unwrap(),
            c
        );
    }

    #[test]
    fn convite_cortado_a_meio_da_erro_legivel() {
        let c = Convite {
            servidor: "00".repeat(16),
            nome: "Casa".into(),
            chave: "11".repeat(32),
            anfitriao: "22".repeat(32),
        };
        let codigo = c.codificar().unwrap();
        let erro = Convite::descodificar(&codigo[..codigo.len() - 10])
            .unwrap_err()
            .to_string();
        assert!(
            erro.contains("incompleto") || erro.contains("corrompido"),
            "o erro tem de dizer a pessoa o que aconteceu, nao um detalhe de base32: {erro}"
        );
    }

    #[test]
    fn texto_que_nao_e_convite_e_recusado_cedo() {
        let erro = Convite::descodificar("ola tudo bem")
            .unwrap_err()
            .to_string();
        assert!(erro.contains("não parece um convite"));
    }
}
