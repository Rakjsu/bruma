//! Confere que a assinatura publicada bate certo com a chave pública que vai DENTRO da app.
//!
//! # Porque é que isto existe
//!
//! A publicação assinava o instalador, copiava a assinatura para o `latest.json`, e conferia
//! que os ficheiros existiam e que a versão batia certo com a etiqueta. Nunca conferia a
//! única coisa de que depende a actualização automática funcionar: que aquela assinatura se
//! verifica com aquela chave.
//!
//! E é a avaria com a pior forma possível — **total e silenciosa**. Uma chave privada rodada
//! sem rodar a pública no `tauri.conf.json` (ou ao contrário) publica uma versão com aspecto
//! perfeito: os ficheiros lá estão, o `latest.json` aponta para o sítio certo, a página da
//! release está bonita. E toda a gente deixa de conseguir actualizar-se, para sempre, sem
//! ver um único erro. Cada versão seguinte agrava, porque ninguém dá por nada.
//!
//! Nenhum dos portões que já existiam podia apanhar isto: todos olham para nomes de
//! ficheiros e números de versão, e a assinatura é o único que precisa da chave.
//!
//! # O formato
//!
//! O Tauri usa minisign, com os ficheiros em base64 por cima:
//!
//! - a `pubkey` do `tauri.conf.json` é o ficheiro `.pub` inteiro em base64;
//! - o `.sig` é o ficheiro de assinatura inteiro em base64.
//!
//! Dentro de cada um, a linha que interessa é a segunda (a primeira é um comentário), também
//! em base64: `algoritmo (2) + id da chave (8) + os bytes`.
//!
//! O algoritmo decide **o que foi assinado**: `Ed` assina o ficheiro como está, `ED` assina o
//! BLAKE2b-512 dele. Tratar os dois é o que distingue esta ferramenta de uma que só funciona
//! por acaso com a versão do `tauri signer` de hoje.

use anyhow::{anyhow, bail, Context, Result};
use blake2::{Blake2b512, Digest};
use data_encoding::BASE64;
use ed25519_dalek::{Signature, VerifyingKey};

/// Descasca uma das duas camadas de base64 e devolve o bloco da segunda linha.
///
/// Devolve também o comentário, que é onde o minisign põe o id da chave em texto — serve
/// para a mensagem de erro dizer alguma coisa de útil a quem estiver a olhar.
fn bloco(b64_do_ficheiro: &str, o_que: &str) -> Result<(Vec<u8>, String)> {
    let dentro = BASE64
        .decode(b64_do_ficheiro.trim().as_bytes())
        .with_context(|| format!("o {o_que} não é base64"))?;
    let texto = String::from_utf8(dentro)
        .with_context(|| format!("o {o_que} não é texto depois de descodificado"))?;
    let mut linhas = texto.lines().filter(|l| !l.trim().is_empty());
    let comentario = linhas.next().unwrap_or_default().to_string();
    let corpo = linhas
        .next()
        .ok_or_else(|| anyhow!("o {o_que} não tem a segunda linha, que é onde estão os bytes"))?;
    let bytes = BASE64
        .decode(corpo.trim().as_bytes())
        .with_context(|| format!("a segunda linha do {o_que} não é base64"))?;
    Ok((bytes, comentario))
}

/// Aceita o `.sig` ou o `latest.json`, e prefere-se sempre o `latest.json`.
///
/// Não é indiferente: o updater lê a assinatura de dentro do `latest.json`, e o ficheiro
/// `.sig` que fica na página da release não é lido por ninguém. Conferir o `.sig` provava um
/// ficheiro decorativo e deixava passar o único que conta — bastava o `latest.json` ficar
/// com a assinatura de uma versão anterior para toda a gente deixar de se actualizar, com o
/// `.sig` certinho ao lado a dizer que estava tudo bem.
fn assinatura_de(caminho: &str) -> Result<(String, String)> {
    let txt =
        std::fs::read_to_string(caminho).with_context(|| format!("não consegui ler {caminho}"))?;
    let Ok(j) = serde_json::from_str::<serde_json::Value>(&txt) else {
        return Ok((txt, format!("ficheiro {caminho}"))); // é o .sig, que é só base64
    };
    let a = j["platforms"]["windows-x86_64"]["signature"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| {
            anyhow!("o {caminho} é JSON mas não tem platforms.windows-x86_64.signature")
        })?;
    Ok((a, format!("campo `signature` do {caminho}")))
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let (exe, sig, conf) = (args.next(), args.next(), args.next());
    let (Some(exe), Some(sig), Some(conf)) = (exe, sig, conf) else {
        bail!("uso: verificar-assinatura <ficheiro> <latest.json|ficheiro.sig> <tauri.conf.json>");
    };

    let conteudo = std::fs::read(&exe).with_context(|| format!("não consegui ler {exe}"))?;
    let (sig_b64, donde) = assinatura_de(&sig)?;
    let conf_txt =
        std::fs::read_to_string(&conf).with_context(|| format!("não consegui ler {conf}"))?;

    let json: serde_json::Value = serde_json::from_str(&conf_txt)?;
    let pubkey_b64 = json["plugins"]["updater"]["pubkey"]
        .as_str()
        .ok_or_else(|| anyhow!("o {conf} não tem plugins.updater.pubkey"))?;

    let (pk, pk_comentario) = bloco(pubkey_b64, "pubkey")?;
    let (sg, sg_comentario) = bloco(&sig_b64, &donde)?;

    if pk.len() != 42 {
        bail!("a chave pública tem {} bytes e deviam ser 42", pk.len());
    }
    if sg.len() != 74 {
        bail!("a assinatura tem {} bytes e deviam ser 74", sg.len());
    }

    let (id_pk, chave) = (&pk[2..10], &pk[10..]);
    let (algo, id_sg, assinatura) = (&sg[..2], &sg[2..10], &sg[10..]);

    // O id da chave PRIMEIRO. Se não bate certo, o erro seguinte seria "assinatura inválida",
    // que manda procurar no sítio errado — o problema não é a assinatura, é ter sido feita
    // com outra chave.
    if id_pk != id_sg {
        bail!(
            "a assinatura foi feita com OUTRA chave.\n  a app espera : {}\n  a assinatura : {}\n\
             \n  Alguém rodou a chave de assinatura sem rodar a `pubkey` do tauri.conf.json\n\
             (ou ao contrário). Ninguém se conseguiria actualizar.",
            pk_comentario.trim(),
            sg_comentario.trim()
        );
    }

    // `Ed` assina o ficheiro; `ED` assina o BLAKE2b-512 dele.
    let alvo: Vec<u8> = match algo {
        b"Ed" => conteudo,
        b"ED" => Blake2b512::digest(&conteudo).to_vec(),
        outro => bail!(
            "não conheço o algoritmo {:?} do minisign",
            String::from_utf8_lossy(outro)
        ),
    };

    let chave: [u8; 32] = chave.try_into()?;
    let assinatura: [u8; 64] = assinatura.try_into()?;
    VerifyingKey::from_bytes(&chave)?
        .verify_strict(&alvo, &Signature::from_bytes(&assinatura))
        .map_err(|e| {
            anyhow!(
                "a assinatura NÃO verifica ({e}).\n  O {exe} e a assinatura do {donde} não \
                 são o mesmo par — ninguém se conseguiria actualizar."
            )
        })?;

    println!(
        "assinatura confere: {} bytes, algoritmo {}, chave {}",
        alvo.len(),
        String::from_utf8_lossy(algo),
        pk_comentario.trim()
    );
    Ok(())
}
