//! Confere que a assinatura publicada bate certo com a chave pública que vai DENTRO da app.
//!
//! # Porque é que isto existe
//!
//! A publicação assinava o instalador, copiava a assinatura para o `latest.json`, e conferia
//! que os ficheiros existiam e que a versão batia certo com a etiqueta. Nunca conferia a
//! única coisa de que depende a actualização automática funcionar: que aquela assinatura se
//! verifica com aquela chave.
//!
//! É a avaria com a pior forma possível — **total e silenciosa**. Uma chave privada rodada
//! sem rodar a pública no `tauri.conf.json` (ou ao contrário) publica uma versão com aspecto
//! perfeito: os ficheiros lá estão, o `latest.json` aponta para o sítio certo, a página da
//! release está bonita. E toda a gente deixa de conseguir actualizar-se, para sempre, sem
//! ver um único erro. Cada versão seguinte agrava, porque ninguém dá por nada.
//!
//! # E porque é que NÃO tem o parsing escrito à mão
//!
//! A primeira versão desta ferramenta lia o formato minisign por sua conta: descascava as
//! duas camadas de base64, pegava na segunda linha, e fazia uma verificação Ed25519. Passava
//! nos testes todos — incluindo os de sabotagem, que eu próprio escrevi.
//!
//! E estava errada. Um ficheiro minisign tem **quatro** linhas: comentário, assinatura,
//! `trusted comment:` e a assinatura GLOBAL, que cobre a assinatura mais o comentário
//! confiável. O `minisign_verify` — que é o que o `tauri-plugin-updater` usa — exige as
//! quatro e faz **duas** verificações. A minha fazia uma e deitava fora metade do ficheiro.
//!
//! Um portão que valida um subconjunto do que o consumidor valida diz «confere» sobre coisas
//! que o consumidor recusa. É pior do que não existir, porque dá confiança a mais.
//!
//! Por isso esta ferramenta usa **a mesma biblioteca, na mesma versão**, chamada da mesma
//! maneira que o updater a chama. Não há aqui um formato reimplementado que possa divergir
//! do verdadeiro: se isto disser que sim, o updater diz que sim.

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use minisign_verify::{PublicKey, Signature};

/// Descasca a camada de base64 com que o Tauri embrulha os ficheiros minisign.
///
/// Tanto a `pubkey` do `tauri.conf.json` como o conteúdo do `.sig` (e o campo `signature` do
/// `latest.json`) são o **ficheiro minisign inteiro** em base64. O que sai daqui vai tal e
/// qual para o `minisign_verify`, que trata do resto — as quatro linhas, os comprimentos, o
/// prefixo do comentário confiável, tudo.
fn desembrulhar(b64: &str, o_que: &str) -> Result<String> {
    let bytes = STANDARD
        .decode(b64.trim().as_bytes())
        .with_context(|| format!("o {o_que} não é base64"))?;
    String::from_utf8(bytes)
        .with_context(|| format!("o {o_que} não é texto depois de descodificado"))
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
        return Ok((txt, format!("ficheiro {caminho}")));
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
    if conteudo.is_empty() {
        bail!("o {exe} está vazio");
    }
    let (sig_b64, donde) = assinatura_de(&sig)?;

    let conf_txt =
        std::fs::read_to_string(&conf).with_context(|| format!("não consegui ler {conf}"))?;
    let json: serde_json::Value = serde_json::from_str(&conf_txt)?;
    let pubkey_b64 = json["plugins"]["updater"]["pubkey"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| anyhow!("o {conf} não tem plugins.updater.pubkey (ou está vazio)"))?;

    // A partir daqui é a MESMA sequência que o `verify_signature` do tauri-plugin-updater.
    let chave = PublicKey::decode(&desembrulhar(pubkey_b64, "pubkey")?)
        .map_err(|e| anyhow!("a `pubkey` do {conf} não é uma chave minisign válida: {e}"))?;
    let assinatura = Signature::decode(&desembrulhar(&sig_b64, &donde)?)
        .map_err(|e| anyhow!("a assinatura do {donde} não é um ficheiro minisign válido: {e}"))?;

    // `true` = aceitar também assinaturas antigas (`Ed`, sobre o ficheiro) além das
    // pré-digeridas (`ED`, sobre o BLAKE2b). É o mesmo valor que o updater passa: aceitar
    // menos do que ele recusaria coisas que ele aceita, e aceitar mais deixaria passar
    // coisas que ele recusa.
    chave.verify(&conteudo, &assinatura, true).map_err(|e| {
        anyhow!(
            "a assinatura NÃO verifica ({e}).\n  O {exe} e a assinatura do {donde} não são o \
             mesmo par — ninguém se conseguiria actualizar."
        )
    })?;

    println!(
        "assinatura confere: {} bytes verificados contra a chave do {conf}, pelo mesmo \
         minisign-verify que o updater usa",
        conteudo.len()
    );
    Ok(())
}
