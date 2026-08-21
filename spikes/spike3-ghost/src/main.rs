//! Spike 3 — Modo Fantasma: chat por onion services, com Tor embutido.
//!
//! Prova (ou desmente):
//!   1. da para alojar um onion service DENTRO da app, com o arti, sem tor externo;
//!   2. sem abrir uma unica porta no router -- os onion services atravessam NAT sozinhos;
//!   3. o peer nunca fica a saber o nosso IP, nem o relay, porque nao ha relay;
//!   4. a cripto e o log sao EXATAMENTE os mesmos do spike 1: muda so o transporte.
//!
//! O ponto 4 e a razao de `spike-common` existir. Se alguma coisa aqui precisasse de uma
//! variante da cripto por causa do Tor, a abstracao de transporte do plano estaria errada.
//!
//! Uso:
//!   spike3-ghost --name ana                          # aloja e imprime o .onion
//!   spike3-ghost --name rui --connect <ENDERECO>.onion

use anyhow::{anyhow, bail, Context, Result};
use arti_client::config::TorClientConfigBuilder;
use arti_client::{TorClient, TorClientConfig};
use data_encoding::HEXLOWER;
use ed25519_dalek::VerifyingKey;
use futures::{AsyncReadExt, AsyncWriteExt, StreamExt};
use safelog::DisplayRedacted;
use serde::{Deserialize, Serialize};
use spike_common::{crypto, log};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{broadcast, Mutex};
use tor_cell::relaycell::msg::Connected;
use tor_hsservice::config::OnionServiceConfigBuilder;
use x25519_dalek::PublicKey as XPublic;

/// Porta virtual dentro do onion service. Nao ha nada aberto no router: isto so existe
/// dentro do circuito Tor.
const PORTA_VIRTUAL: u16 = 9001;
const MAX_FRAME: usize = 16 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(tag = "t")]
enum Msg {
    Hello {
        id: String,
        x_pub: String,
        prekey_sig: String,
    },
    Sync {
        entries: Vec<log::Entry>,
    },
    New {
        entry: log::Entry,
    },
}

struct Shared {
    ident: crypto::Identity,
    store: Mutex<log::Log>,
    key: Mutex<Option<[u8; 32]>>,
    tx: broadcast::Sender<log::Entry>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // O rustls 0.23 recusa-se a escolher sozinho um provedor de cripto quando ha mais do
    // que um na arvore de dependencias, e entra em panico a meio do arranque do Tor. Tem
    // de ser escolhido aqui, antes de tudo. Ignora-se o erro porque so falha se ja estiver
    // instalado, o que nao e problema.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let (name, connect_to, verbose) = parse_args()?;

    // O bootstrap do Tor pode demorar minutos e falhar em silencio. Com --verbose
    // ve-se exatamente onde e que fica preso.
    if verbose {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                    "info,arti=debug,tor_dirmgr=debug,tor_guardmgr=debug".into()
                }),
            )
            .init();
    }

    std::fs::create_dir_all("data")?;
    let seed = load_or_create_seed(&format!("data/{name}.key"))?;
    let ident = crypto::Identity::from_seed(&seed);
    let log_path = format!("data/{name}-log.json");
    let store = log::Log::load(&log_path)?;

    let (tx, _) = broadcast::channel(256);
    let shared = Arc::new(Shared {
        ident,
        store: Mutex::new(store),
        key: Mutex::new(None),
        tx,
    });

    println!("--- Bruma . spike 3 . Modo Fantasma ---");
    println!("perfil     : {name}");
    println!(
        "identidade : {}",
        HEXLOWER.encode(shared.ident.verifying().as_bytes())
    );
    println!(
        "log local  : {log_path} ({} entradas)",
        shared.store.lock().await.len()
    );

    // O arti precisa de um sitio para o estado e para o keystore. Fica dentro de data/
    // para o spike ser descartavel de uma so vez.
    let state = format!("data/{name}-tor/state");
    let cache = format!("data/{name}-tor/cache");
    std::fs::create_dir_all(&state)?;
    std::fs::create_dir_all(&cache)?;
    let config: TorClientConfig = TorClientConfigBuilder::from_directories(&state, &cache)
        .build()
        .map_err(|e| anyhow!("config do arti: {e}"))?;

    println!("tor        : a arrancar e a construir circuitos (pode levar 30-60 s)...");
    let t0 = std::time::Instant::now();
    let client = TorClient::builder()
        .config(config)
        .create_bootstrapped()
        .await
        .map_err(|e| anyhow!("bootstrap do Tor falhou: {e}"))?;
    println!("tor        : pronto em {:?}", t0.elapsed());
    println!("---------------------------------------");

    spawn_stdin(shared.clone());

    match connect_to {
        Some(endereco) => ligar(&client, &endereco, shared).await,
        None => alojar(&client, &name, shared).await,
    }
}

/// Lado que aloja: publica um onion service e aceita ligacoes por dentro do Tor.
async fn alojar(
    client: &TorClient<tor_rtcompat::PreferredRuntime>,
    name: &str,
    shared: Arc<Shared>,
) -> Result<()> {
    let nickname = name.parse().map_err(|_| {
        anyhow!("'{name}' nao serve como nickname de onion service (usa letras e digitos)")
    })?;
    let cfg = OnionServiceConfigBuilder::default()
        .nickname(nickname)
        .build()
        .map_err(|e| anyhow!("config do onion service: {e}"))?;

    // Devolve Option: e None se ja existir um servico com este nickname a correr.
    let (service, rend_requests) = client
        .launch_onion_service(cfg)
        .map_err(|e| anyhow!("nao consegui alojar o onion service: {e}"))?
        .ok_or_else(|| anyhow!("ja existe um onion service com o nickname '{name}' a correr"))?;

    let endereco = service
        .onion_address()
        .ok_or_else(|| anyhow!("o servico arrancou mas nao tem endereco"))?;
    // O arti esconde enderecos .onion nos logs de proposito. Aqui queremo-lo inteiro,
    // porque o utilizador precisa de o copiar.
    let endereco = endereco.display_unredacted().to_string();

    println!();
    println!("[ok] Onion service no ar, SEM abrir portas no router.");
    println!("     Da este comando ao outro lado:");
    println!("         spike3-ghost --name <outro> --connect {endereco}");
    println!();
    println!("     Nota: publicar o descritor na rede Tor pode levar mais 30-60 s.");
    println!("     Se o outro lado falhar a primeira tentativa, espera e tenta de novo.");
    println!();

    let mut pedidos = Box::pin(tor_hsservice::handle_rend_requests(rend_requests));
    while let Some(pedido) = pedidos.next().await {
        // O spike aceita qualquer porta virtual: filtrar so acrescentaria ruido a um teste
        // que quer responder "isto liga ou nao liga".
        let stream = match pedido.accept(Connected::new_empty()).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[!] falha a aceitar ligacao: {e}");
                continue;
            }
        };
        println!("[ok] Ligacao recebida por dentro do Tor.");
        let (leitura, escrita) = stream.split();
        if let Err(e) = sessao(leitura, escrita, shared.clone()).await {
            eprintln!("[sessao terminou: {e}]");
        }
        println!("[a espera de nova ligacao...]");
    }
    Ok(())
}

/// Lado que se liga: fala com o .onion atraves de um circuito Tor.
async fn ligar(
    client: &TorClient<tor_rtcompat::PreferredRuntime>,
    endereco: &str,
    shared: Arc<Shared>,
) -> Result<()> {
    let endereco = endereco.trim().trim_end_matches('/');
    if !endereco.ends_with(".onion") {
        bail!("'{endereco}' nao parece um endereco .onion");
    }
    println!("A ligar a {endereco}:{PORTA_VIRTUAL} pelo Tor (pode levar 30 s)...");
    let t0 = std::time::Instant::now();
    let stream = client
        .connect((endereco, PORTA_VIRTUAL))
        .await
        .map_err(|e| anyhow!("nao consegui ligar ao onion service: {e}"))?;
    println!("[ok] Circuito estabelecido em {:?}", t0.elapsed());

    let (leitura, escrita) = stream.split();
    sessao(leitura, escrita, shared).await
}

/// Handshake, sync e conversa. Identico em espirito ao spike 1 -- so o tipo do stream muda.
async fn sessao<R, W>(mut leitura: R, mut escrita: W, shared: Arc<Shared>) -> Result<()>
where
    R: futures::AsyncRead + Unpin + Send + 'static,
    W: futures::AsyncWrite + Unpin,
{
    // Aqui NAO ha certificado a autenticar o peer, ao contrario do iroh: o Tor autentica o
    // ENDERECO do servico, nao a identidade do utilizador. Por isso a identidade vem no Hello
    // e a assinatura da prekey e que a prova.
    escrever(
        &mut escrita,
        &Msg::Hello {
            id: HEXLOWER.encode(shared.ident.verifying().as_bytes()),
            x_pub: HEXLOWER.encode(shared.ident.x_public().as_bytes()),
            prekey_sig: HEXLOWER.encode(&shared.ident.prekey_signature()),
        },
    )
    .await?;

    let (id, x_pub, prekey_sig) = match ler(&mut leitura).await? {
        Msg::Hello {
            id,
            x_pub,
            prekey_sig,
        } => (id, x_pub, prekey_sig),
        _ => bail!("esperava Hello como primeira mensagem"),
    };
    let peer_id = decode_n::<32>(&id)?;
    let peer_key =
        VerifyingKey::from_bytes(&peer_id).map_err(|_| anyhow!("identidade do peer invalida"))?;
    let peer_x = decode_n::<32>(&x_pub)?;
    crypto::verify_prekey(&peer_key, &peer_x, &decode_n::<64>(&prekey_sig)?)?;

    let key = crypto::session_key(
        &shared.ident.x_secret,
        &XPublic::from(peer_x),
        shared.ident.verifying().as_bytes(),
        &peer_id,
    );
    *shared.key.lock().await = Some(key);
    println!("[ok] Peer {} verificado; chave de sessao pronta.", &id[..8]);
    println!();

    let meu = shared.store.lock().await.ordered();
    let enviei = meu.len();
    escrever(&mut escrita, &Msg::Sync { entries: meu }).await?;

    let entries = match ler(&mut leitura).await? {
        Msg::Sync { entries } => entries,
        _ => bail!("esperava Sync"),
    };
    let recebi = entries.len();
    let novas = shared.store.lock().await.merge(entries)?;
    println!("--- sync: enviei {enviei}, recebi {recebi}, {novas} eram novas ---");
    for e in shared.store.lock().await.ordered() {
        mostrar(&key, &e);
    }
    println!("--- escreve e carrega Enter ---");
    println!();

    let rx_shared = shared.clone();
    let mut leitor = tokio::spawn(async move {
        loop {
            match ler(&mut leitura).await {
                Ok(Msg::New { entry }) => {
                    let mut g = rx_shared.store.lock().await;
                    if g.merge(vec![entry.clone()]).unwrap_or(0) > 0 {
                        mostrar(&key, &entry);
                    }
                }
                Ok(_) => eprintln!("[!] mensagem inesperada, ignorada"),
                Err(_) => break,
            }
        }
    });

    let mut sub = shared.tx.subscribe();
    loop {
        tokio::select! {
            _ = &mut leitor => break,
            got = sub.recv() => match got {
                Ok(entry) => {
                    if let Err(e) = escrever(&mut escrita, &Msg::New { entry }).await {
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
    leitor.abort();
    Ok(())
}

fn spawn_stdin(shared: Arc<Shared>) {
    tokio::spawn(async move {
        let mut linhas = BufReader::new(tokio::io::stdin()).lines();
        while let Ok(Some(linha)) = linhas.next_line().await {
            if linha.trim().is_empty() {
                continue;
            }
            let talvez = *shared.key.lock().await;
            let Some(key) = talvez else {
                eprintln!("[!] ainda sem chave -- espera pela ligacao");
                continue;
            };
            let (nonce, ct) = match crypto::seal(&key, linha.as_bytes()) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("[!] falha a cifrar: {e}");
                    continue;
                }
            };
            let entrada = {
                let mut g = shared.store.lock().await;
                match g.append_local(&shared.ident.signing, nonce, ct, agora_ms()) {
                    Ok(e) => e,
                    Err(e) => {
                        eprintln!("[!] falha a gravar: {e}");
                        continue;
                    }
                }
            };
            let _ = shared.tx.send(entrada);
        }
    });
}

fn mostrar(key: &[u8; 32], e: &log::Entry) {
    let quem = &e.author[..8];
    let corpo = HEXLOWER
        .decode(e.ciphertext.as_bytes())
        .ok()
        .and_then(|ct| {
            let n = decode_n::<24>(&e.nonce).ok()?;
            crypto::open(key, &n, &ct).ok()
        })
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_else(|| "<nao decifravel>".to_string());
    println!("  [{quem}] {corpo}");
}

// Enquadramento: u32 big-endian + JSON, sobre qualquer AsyncRead/AsyncWrite.

async fn escrever<W: futures::AsyncWrite + Unpin>(w: &mut W, m: &Msg) -> Result<()> {
    let corpo = serde_json::to_vec(m)?;
    w.write_all(&(corpo.len() as u32).to_be_bytes()).await?;
    w.write_all(&corpo).await?;
    w.flush().await?;
    Ok(())
}

async fn ler<R: futures::AsyncRead + Unpin>(r: &mut R) -> Result<Msg> {
    let mut tam = [0u8; 4];
    r.read_exact(&mut tam).await?;
    let n = u32::from_be_bytes(tam) as usize;
    if n > MAX_FRAME {
        bail!("frame de {n} bytes excede o limite");
    }
    let mut corpo = vec![0u8; n];
    r.read_exact(&mut corpo).await?;
    Ok(serde_json::from_slice(&corpo)?)
}

// Auxiliares.

fn parse_args() -> Result<(String, Option<String>, bool)> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut name = "peer".to_string();
    let mut connect_to = None;
    let mut verbose = false;
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
                        .ok_or_else(|| anyhow!("--connect precisa de um endereco .onion"))?,
                );
                i += 2;
            }
            "--verbose" | "-v" => {
                verbose = true;
                i += 1;
            }
            "--help" | "-h" => {
                println!("spike3-ghost --name <perfil> [--connect <ENDERECO>.onion] [--verbose]");
                std::process::exit(0);
            }
            outro => bail!("argumento desconhecido: {outro}"),
        }
    }
    Ok((name, connect_to, verbose))
}

fn load_or_create_seed(path: &str) -> Result<[u8; 32]> {
    if let Ok(raw) = std::fs::read(path) {
        if raw.len() == 32 {
            let mut s = [0u8; 32];
            s.copy_from_slice(&raw);
            return Ok(s);
        }
        bail!("{path} existe mas nao tem 32 bytes");
    }
    let mut s = [0u8; 32];
    getrandom::getrandom(&mut s).map_err(|e| anyhow!("rng: {e}"))?;
    std::fs::write(path, s)?;
    println!("(semente nova criada em {path})");
    Ok(s)
}

fn agora_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn decode_n<const N: usize>(s: &str) -> Result<[u8; N]> {
    let v = HEXLOWER.decode(s.as_bytes()).context("hex invalido")?;
    if v.len() != N {
        bail!("esperava {N} bytes, recebi {}", v.len());
    }
    let mut o = [0u8; N];
    o.copy_from_slice(&v);
    Ok(o)
}
