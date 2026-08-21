//! Spike 1 — a premissa serverless do Bruma.
//!
//! Prova (ou desmente) quatro coisas, sem servidor nenhum:
//!   1. dois PCs em REDES DIFERENTES ligam-se so pela chave publica;
//!   2. a ligacao e direta (hole-punch) ou cai no relay  <- o veredito de CGNAT;
//!   3. o conteudo trocado e opaco (ver o ficheiro data/*-log.json);
//!   4. um peer fecha, o outro escreve, e ao voltar sincroniza o que perdeu.
//!
//! O ponto 4 obriga a poder escrever SEM ligacao. Por isso o ciclo de stdin corre sempre,
//! escreve no log local, e a sessao (quando existe) limita-se a difundir o que aparecer.
//! A chave e guardada em data/<perfil>-peer.json depois do primeiro handshake, para se
//! poder cifrar offline. Num produto isto seria a chave de epoca do canal, que existe
//! independentemente de haver ou nao ligacao.
//!
//! Uso:
//!   spike1-net --name ana                       # imprime o ID e fica a espera
//!   spike1-net --name rui --connect <ENDPOINT_ID>

use spike_common::{crypto, log};

use anyhow::{anyhow, bail, Context, Result};
use data_encoding::HEXLOWER;
use ed25519_dalek::VerifyingKey;
use iroh::endpoint::{presets, Connection, RecvStream, SendStream};
use iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{broadcast, Mutex};
use x25519_dalek::PublicKey as XPublic;

const ALPN: &[u8] = b"bruma/spike1/0";
const MAX_FRAME: usize = 16 * 1024 * 1024;
/// Antes do handshake o outro lado e so "alguem que sabe o nosso endereco". Aceitar
/// um prefixo de 16 MiB dele significa alocar 16 MiB a pedido de um desconhecido,
/// repetidamente. Depois de a identidade estar provada, o limite normal aplica-se.
const MAX_FRAME_PRE_HANDSHAKE: usize = 64 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(tag = "t")]
enum Msg {
    Hello { x_pub: String, prekey_sig: String },
    Sync { entries: Vec<log::Entry> },
    New { entry: log::Entry },
}

/// O que se guarda do peer para conseguir cifrar mesmo sem ele estar ligado.
#[derive(Serialize, Deserialize)]
struct PeerCache {
    endpoint_id: String,
    x_pub: String,
}

struct Shared {
    ident: crypto::Identity,
    store: Mutex<log::Log>,
    key: Mutex<Option<[u8; 32]>>,
    tx: broadcast::Sender<log::Entry>,
    peer_path: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let (name, connect_to) = parse_args()?;

    std::fs::create_dir_all("data")?;
    let seed = load_or_create_seed(&format!("data/{name}.key"))?;
    let ident = crypto::Identity::from_seed(&seed);
    let log_path = format!("data/{name}-log.json");
    let peer_path = format!("data/{name}-peer.json");
    let store = log::Log::load(&log_path)?;

    let (tx, _) = broadcast::channel(256);
    let shared = Arc::new(Shared {
        ident,
        store: Mutex::new(store),
        key: Mutex::new(None),
        tx,
        peer_path,
    });

    // Se ja falamos com este peer antes, da para cifrar offline desde ja.
    let cached = restore_cached_key(&shared).await;

    let ep = Endpoint::builder(presets::N0)
        .secret_key(SecretKey::from_bytes(&seed))
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await
        .map_err(|e| anyhow!("bind falhou: {e}"))?;

    println!("--- Bruma . spike 1 ---");
    println!("perfil     : {name}");
    println!("identidade : {}", ep.id());
    println!(
        "log local  : {log_path} ({} entradas)",
        shared.store.lock().await.len()
    );
    println!(
        "chave      : {}",
        if cached {
            "recuperada da cache -- ja podes escrever offline"
        } else {
            "ainda nenhuma -- liga-te uma vez primeiro"
        }
    );
    println!("relay      : a ligar...");
    ep.online().await;
    println!("relay      : pronto");
    println!("-----------------------");

    // O stdin corre SEMPRE, haja ou nao ligacao. E isto que torna o ponto 4 testavel.
    spawn_stdin(shared.clone());

    match connect_to {
        Some(s) => {
            let peer: EndpointId = s.trim().parse().context("EndpointId invalido")?;
            println!("A ligar a {peer} ...");
            let conn = ep
                .connect(EndpointAddr::from(peer), ALPN)
                .await
                .map_err(|e| anyhow!("ligacao falhou: {e}"))?;
            let (send, recv) = conn.open_bi().await.map_err(|e| anyhow!("open_bi: {e}"))?;
            session(conn, send, recv, shared).await?;
            println!("[sessao terminada]");
        }
        None => {
            println!("A espera de ligacao. Da este comando ao outro lado:");
            println!("    spike1-net --name <outro> --connect {}", ep.id());
            // Ciclo: assim o peer pode desligar e voltar sem reiniciar este lado.
            loop {
                let Some(incoming) = ep.accept().await else {
                    break;
                };
                let conn = match incoming.await {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("[!] accept falhou: {e}");
                        continue;
                    }
                };
                let (send, recv) = match conn.accept_bi().await {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("[!] accept_bi falhou: {e}");
                        continue;
                    }
                };
                if let Err(e) = session(conn, send, recv, shared.clone()).await {
                    eprintln!("[sessao terminou: {e}]");
                }
                println!("[a espera de nova ligacao...]");
            }
        }
    }
    Ok(())
}

async fn session(
    conn: Connection,
    mut send: SendStream,
    mut recv: RecvStream,
    shared: Arc<Shared>,
) -> Result<()> {
    // A identidade do peer vem do certificado TLS: autenticada pelo transporte, nao por nos.
    let peer_id = conn.remote_id();
    let peer_key = VerifyingKey::from_bytes(peer_id.as_bytes())
        .map_err(|_| anyhow!("EndpointId nao e uma chave Ed25519 valida"))?;
    println!();
    println!("[ok] Ligado a {peer_id}");
    report_paths(&conn, "inicial");
    spawn_path_monitor(conn.clone());

    // Handshake: troca de prekeys X25519, cada uma assinada pela identidade.
    write_msg(
        &mut send,
        &Msg::Hello {
            x_pub: HEXLOWER.encode(shared.ident.x_public().as_bytes()),
            prekey_sig: HEXLOWER.encode(&shared.ident.prekey_signature()),
        },
    )
    .await?;

    let (x_pub, prekey_sig) = match read_msg(&mut recv, MAX_FRAME_PRE_HANDSHAKE).await? {
        Msg::Hello { x_pub, prekey_sig } => (x_pub, prekey_sig),
        _ => bail!("esperava Hello como primeira mensagem"),
    };
    let peer_x = decode_n::<32>(&x_pub)?;
    crypto::verify_prekey(&peer_key, &peer_x, &decode_n::<64>(&prekey_sig)?)?;

    let key = crypto::session_key(
        &shared.ident.x_secret,
        &XPublic::from(peer_x),
        shared.ident.verifying().as_bytes(),
        peer_id.as_bytes(),
    );
    *shared.key.lock().await = Some(key);
    save_peer_cache(&shared.peer_path, &peer_id.to_string(), &x_pub)?;
    println!("[ok] Chave de sessao estabelecida (prekey assinada e verificada)");
    println!();

    // Sync: troca do log inteiro. Simples de proposito; o produto trocara so o delta.
    let mine = shared.store.lock().await.ordered();
    let enviei = mine.len();
    write_msg(&mut send, &Msg::Sync { entries: mine }).await?;

    let entries = match read_msg(&mut recv, MAX_FRAME).await? {
        Msg::Sync { entries } => entries,
        _ => bail!("esperava Sync"),
    };
    let recebi = entries.len();
    let novas = shared.store.lock().await.merge(entries)?;
    println!("--- sync: enviei {enviei}, recebi {recebi}, {novas} eram novas ---");
    for e in shared.store.lock().await.ordered() {
        print_entry(&key, &e);
    }
    println!("--- fim do historico . escreve e carrega Enter ---");
    println!();

    // Rececao continua.
    let rx_shared = shared.clone();
    let mut reader = tokio::spawn(async move {
        loop {
            match read_msg(&mut recv, MAX_FRAME).await {
                Ok(Msg::New { entry }) => {
                    let mut g = rx_shared.store.lock().await;
                    if g.merge(vec![entry.clone()]).unwrap_or(0) > 0 {
                        print_entry(&key, &entry);
                    }
                }
                Ok(_) => eprintln!("[!] mensagem inesperada, ignorada"),
                Err(_) => break,
            }
        }
    });

    // Difusao: o stdin escreve no log e avisa aqui; esta tarefa so reencaminha.
    let mut sub = shared.tx.subscribe();
    loop {
        tokio::select! {
            _ = &mut reader => break,
            got = sub.recv() => match got {
                Ok(entry) => {
                    if let Err(e) = write_msg(&mut send, &Msg::New { entry }).await {
                        eprintln!("[!] nao consegui enviar: {e} -- ficou guardado localmente");
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    eprintln!("[!] {n} entradas perdidas na difusao; o proximo sync recupera-as");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
        }
    }
    reader.abort();
    Ok(())
}

/// Le stdin em continuo. Funciona sem ligacao — e esse o ponto.
fn spawn_stdin(shared: Arc<Shared>) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if line.trim().is_empty() {
                continue;
            }
            let maybe_key = *shared.key.lock().await;
            let Some(key) = maybe_key else {
                eprintln!("[!] ainda nao ha chave -- liga-te uma vez antes de escrever");
                continue;
            };
            let entry = {
                let (nonce, ct) = match crypto::seal(&key, line.as_bytes()) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("[!] falha a cifrar: {e}");
                        continue;
                    }
                };
                let mut g = shared.store.lock().await;
                match g.append_local(&shared.ident.signing, nonce, ct, now_ms()) {
                    Ok(e) => e,
                    Err(e) => {
                        eprintln!("[!] falha a gravar: {e}");
                        continue;
                    }
                }
            };
            // Sem ligacao isto falha em silencio, e esta certo: fica no log e vai no proximo sync.
            let _ = shared.tx.send(entry);
        }
    });
}

async fn restore_cached_key(shared: &Arc<Shared>) -> bool {
    let Ok(raw) = std::fs::read_to_string(&shared.peer_path) else {
        return false;
    };
    let Ok(cache) = serde_json::from_str::<PeerCache>(&raw) else {
        return false;
    };
    let (Ok(peer_id), Ok(x)) = (
        cache.endpoint_id.parse::<EndpointId>(),
        decode_n::<32>(&cache.x_pub),
    ) else {
        return false;
    };
    let key = crypto::session_key(
        &shared.ident.x_secret,
        &XPublic::from(x),
        shared.ident.verifying().as_bytes(),
        peer_id.as_bytes(),
    );
    *shared.key.lock().await = Some(key);
    true
}

fn save_peer_cache(path: &str, endpoint_id: &str, x_pub: &str) -> Result<()> {
    let cache = PeerCache {
        endpoint_id: endpoint_id.to_string(),
        x_pub: x_pub.to_string(),
    };
    std::fs::write(path, serde_json::to_string_pretty(&cache)?)?;
    Ok(())
}

/// O veredito do spike: direto = hole-punch funcionou; relay = CGNAT ou NAT hostil.
fn report_paths(conn: &Connection, when: &str) {
    let paths = conn.paths();
    match paths.iter().find(|p| p.is_selected()) {
        Some(p) if p.is_relay() => {
            println!("  [!] caminho {when}: RELAY (hole-punch nao passou -- sinal de CGNAT)");
        }
        Some(p) if p.is_ip() => {
            println!("  [ok] caminho {when}: DIRETO ({:?})", p.remote_addr());
        }
        Some(_) => println!("  [..] caminho {when}: transporte alternativo"),
        None => println!(
            "  [..] caminho {when}: ainda a decidir ({} candidatos)",
            paths.len()
        ),
    }
}

/// O hole-punch acontece DEPOIS da ligacao abrir. Sem vigiar isto, mede-se sempre "relay".
fn spawn_path_monitor(conn: Connection) {
    tokio::spawn(async move {
        let mut was_direct = false;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            let paths = conn.paths();
            let Some(p) = paths.iter().find(|p| p.is_selected()) else {
                continue;
            };
            let direct = p.is_ip();
            if direct != was_direct {
                was_direct = direct;
                if direct {
                    println!(
                        "  [ok] passou a DIRETO ({:?}) -- hole-punch feito",
                        p.remote_addr()
                    );
                } else {
                    println!("  [!] voltou para RELAY");
                }
            }
        }
    });
}

fn print_entry(key: &[u8; 32], e: &log::Entry) {
    let who = &e.author[..8];
    let body = HEXLOWER
        .decode(e.ciphertext.as_bytes())
        .ok()
        .and_then(|ct| {
            let n = decode_n::<24>(&e.nonce).ok()?;
            crypto::open(key, &n, &ct).ok()
        })
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_else(|| "<nao decifravel com esta chave>".to_string());
    println!("  [{who}] {body}");
}

// Enquadramento: u32 big-endian + JSON.

async fn write_msg(send: &mut SendStream, m: &Msg) -> Result<()> {
    let body = serde_json::to_vec(m)?;
    send.write_all(&(body.len() as u32).to_be_bytes())
        .await
        .map_err(|e| anyhow!("write: {e}"))?;
    send.write_all(&body)
        .await
        .map_err(|e| anyhow!("write: {e}"))?;
    Ok(())
}

async fn read_msg(recv: &mut RecvStream, limite: usize) -> Result<Msg> {
    let mut len = [0u8; 4];
    recv.read_exact(&mut len)
        .await
        .map_err(|e| anyhow!("read: {e}"))?;
    let n = u32::from_be_bytes(len) as usize;
    if n > limite {
        bail!("frame de {n} bytes excede o limite de {limite}");
    }
    let mut body = vec![0u8; n];
    recv.read_exact(&mut body)
        .await
        .map_err(|e| anyhow!("read: {e}"))?;
    Ok(serde_json::from_slice(&body)?)
}

// Auxiliares.

fn parse_args() -> Result<(String, Option<String>)> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut name = "peer".to_string();
    let mut connect_to = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--name" => {
                name = args
                    .get(i + 1)
                    .cloned()
                    .ok_or_else(|| anyhow!("--name precisa de um valor"))?;
                i += 2;
            }
            "--connect" => {
                connect_to = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| anyhow!("--connect precisa de um EndpointId"))?,
                );
                i += 2;
            }
            "--help" | "-h" => {
                println!("spike1-net --name <perfil> [--connect <ENDPOINT_ID>]");
                std::process::exit(0);
            }
            other => bail!("argumento desconhecido: {other}"),
        }
    }
    Ok((name, connect_to))
}

fn load_or_create_seed(path: &str) -> Result<[u8; 32]> {
    if let Ok(raw) = std::fs::read(path) {
        if raw.len() == 32 {
            let mut s = [0u8; 32];
            s.copy_from_slice(&raw);
            return Ok(s);
        }
        bail!("{path} existe mas nao tem 32 bytes -- apaga-o ou usa outro --name");
    }
    let mut s = [0u8; 32];
    getrandom::getrandom(&mut s).map_err(|e| anyhow!("rng: {e}"))?;
    std::fs::write(path, s)?;
    println!("(semente nova criada em {path})");
    Ok(s)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn decode_n<const N: usize>(s: &str) -> Result<[u8; N]> {
    let v = HEXLOWER.decode(s.as_bytes())?;
    if v.len() != N {
        bail!("esperava {N} bytes, recebi {}", v.len());
    }
    let mut o = [0u8; N];
    o.copy_from_slice(&v);
    Ok(o)
}
