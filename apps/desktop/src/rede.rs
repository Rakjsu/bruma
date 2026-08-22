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
        para: String,
        servidor: String,
        canal: String,
        dados: Arc<Vec<u8>>,
    },
}

pub struct Rede {
    pub endpoint: Endpoint,
    /// Entradas criadas localmente, para as sessões abertas difundirem.
    pub tx: broadcast::Sender<Saida>,
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
                                if let Err(e) = sessao(conn, rede, app, janela).await {
                                    eprintln!("[rede] sessão terminou: {e}");
                                }
                            }
                            Err(e) => eprintln!("[rede] ligação recusada: {e}"),
                        }
                    });
                }
            });
        }

        // Reatar com os peers já conhecidos, sem precisar do convite outra vez.
        {
            let rede = rede.clone();
            let app = app.clone();
            let janela = janela.clone();
            tokio::spawn(async move {
                let conhecidos: Vec<String> = {
                    let s = app.servidores.lock().unwrap();
                    let mut v: Vec<String> = s.values().flat_map(|x| x.peers.clone()).collect();
                    v.sort();
                    v.dedup();
                    v
                };
                for p in conhecidos {
                    let _ = ligar(&rede, &app, &janela, &p).await;
                }
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
        let _ = self.tx.send(Saida::Presenca(servidor.to_string(), canal));
    }

    /// Um pedaco de ecra para cada espectador. Quem nao carregou em "Assistir" nao
    /// aparece nesta lista e nao recebe nada -- e a diferenca entre gastar uma copia de
    /// upload e gastar seis.
    pub fn enviar_video(&self, para: &[String], servidor: &str, canal: &str, dados: Arc<Vec<u8>>) {
        for p in para {
            let _ = self.tx.send(Saida::Video {
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

pub async fn ligar(rede: &Arc<Rede>, app: &Arc<App>, janela: &AppHandle, peer: &str) -> Result<()> {
    let id: EndpointId = peer
        .trim()
        .parse()
        .map_err(|_| anyhow!("identificador de peer inválido"))?;
    if id == rede.endpoint.id() {
        bail!("esse é o teu próprio identificador");
    }
    let conn = rede
        .endpoint
        .connect(EndpointAddr::from(id), ALPN)
        .await
        .map_err(|e| anyhow!("não consegui ligar: {e}"))?;
    let (rede, app, janela) = (rede.clone(), app.clone(), janela.clone());
    tokio::spawn(async move {
        if let Err(e) = sessao(conn, rede, app, janela).await {
            eprintln!("[rede] sessão terminou: {e}");
        }
    });
    Ok(())
}

async fn sessao(conn: Connection, rede: Arc<Rede>, app: Arc<App>, janela: AppHandle) -> Result<()> {
    // Quem abre a ligação abre também o stream; quem aceita espera por ele.
    let (mut envia, mut recebe) = match conn.open_bi().await {
        Ok(par) => par,
        Err(_) => conn
            .accept_bi()
            .await
            .map_err(|e| anyhow!("sem stream: {e}"))?,
    };

    let peer = conn.remote_id().to_string();
    let _ = janela.emit("peer-ligado", &peer);

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
                    servidor,
                    canal,
                    dados,
                }) => {
                    crate::comandos::ecra_recebido(&peer_leitura, &servidor, &canal, dados);
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
                        Saida::Video { para, servidor, canal, dados } if para == peer => {
                            Some(Quadro::Video { servidor, canal, dados: dados.as_ref().clone() })
                        }
                        Saida::Video { .. } => None,
                    };
                    if let Some(q) = quadro {
                        if escrever_quadro(&mut envia, &q).await.is_err() {
                            break;
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    // Perdeu-se difusão; o próximo sync recupera. Não vale a pena cair.
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
        }
    }
    leitor.abort();
    let _ = janela.emit("peer-desligado", &peer);
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
        servidor: String,
        canal: String,
        dados: Vec<u8>,
    },
}

#[derive(Serialize, Deserialize)]
struct CabecalhoVideo {
    servidor: String,
    canal: String,
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
            servidor,
            canal,
            dados,
        } => {
            let cab = serde_json::to_vec(&CabecalhoVideo {
                servidor: servidor.clone(),
                canal: canal.clone(),
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
                servidor: cab.servidor,
                canal: cab.canal,
                dados: corpo[fim..].to_vec(),
            })
        }
        outro => bail!("tipo de quadro desconhecido: {outro}"),
    }
}
