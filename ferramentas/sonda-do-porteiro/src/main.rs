//! Sonda do porteiro do Bruma: liga-se e NÃO colabora.
//!
//! # Porque é que isto existe, e porque é que está no repositório
//!
//! Um par mal-intencionado não é uma bandeira de ambiente nossa — é software de outra pessoa.
//! As bandeiras `BRUMA_ESTRANHO` e companhia simulam um atacante que corre o NOSSO código, e
//! por isso só conseguem fazer o que o nosso código sabe fazer. Esta sonda não: fala o
//! protocolo do lado de fora e recusa-se a cumprir a parte dela.
//!
//! Nasceu fora do repositório, com o argumento de que assim nunca se poderia esquecer de ser
//! removida. O argumento não se aguentou: na primeira utilização apanhou um defeito que
//! nenhum teste tinha apanhado — os dois porteiros do prazo e do tecto estavam DEPOIS de uma
//! espera que nunca termina para quem não abre stream nenhum, e oito ligações mudas ficaram
//! de pé 45 segundos sem uma linha no registo. Uma ferramenta que faz isso não pode viver
//! numa pasta temporária.
//!
//! E o receio era infundado: isto é um binário separado, como o `verificar-assinatura` e o
//! `so-o-que-vai-na-release`. Nunca é ligado à app nem ao instalador, portanto não há nada
//! que se possa esquecer de tirar de lado nenhum.
//!
//! # Como se usa
//!
//! Arranca-se o Bruma e lê-se o `endereço` que ele imprime; depois:
//!
//! ```text
//! cargo run -p sonda-do-porteiro -- <endereço> mudo
//! cargo run -p sonda-do-porteiro -- <endereço> enxame 8
//! ```
//!
//! O que se mede é quanto tempo cada ligação sobrevive. Uma que fica de pé para sempre é o
//! defeito; uma que é fechada, com a razão a dizer qual porteiro a fechou, é a defesa.

use anyhow::Result;
use iroh::{endpoint::Endpoint, EndpointAddr, EndpointId};

const ALPN: &[u8] = b"bruma/1";

async fn ligar_e_calar(alvo: EndpointId, etiqueta: String, limite_s: u64) -> Result<()> {
    let ep = Endpoint::builder(iroh::endpoint::presets::N0)
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await?;
    let inicio = std::time::Instant::now();
    let conn = match ep.connect(EndpointAddr::from(alvo), ALPN).await {
        Ok(c) => c,
        Err(e) => {
            println!(
                "{etiqueta}: NÃO LIGUEI ({e}) ao fim de {:?}",
                inicio.elapsed()
            );
            return Ok(());
        }
    };
    println!(
        "{etiqueta}: ligado em {:?}; agora fico calado",
        inicio.elapsed()
    );

    // Nem um byte. Nem sequer abrir um stream: é o pior caso para o outro lado, porque uma
    // ligação QUIC viva sem tráfego não dá erro nenhum a quem espera.
    let fechou =
        tokio::time::timeout(std::time::Duration::from_secs(limite_s), conn.closed()).await;
    match fechou {
        Ok(razao) => println!(
            "{etiqueta}: FECHARAM-ME ao fim de {:?} — razão: {razao:?}",
            inicio.elapsed()
        ),
        Err(_) => {
            println!("{etiqueta}: AINDA DE PÉ ao fim de {limite_s}s — o porteiro não fez nada")
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let alvo: EndpointId = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("uso: sonda <endpoint-id> mudo|enxame <n>"))?
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("endpoint-id inválido"))?;
    let modo = args.next().unwrap_or_else(|| "mudo".into());
    let quantas: usize = args.next().and_then(|n| n.parse().ok()).unwrap_or(8);

    match modo.as_str() {
        "mudo" => ligar_e_calar(alvo, "mudo".into(), 45).await?,
        "enxame" => {
            let mut tarefas = Vec::new();
            for i in 0..quantas {
                tarefas.push(tokio::spawn(ligar_e_calar(alvo, format!("#{i}"), 45)));
                // Escalonadas, para o outro lado as ver chegar uma a uma e o tecto ter
                // sentido: todas ao mesmo instante mediria uma corrida, não uma regra.
                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            }
            for t in tarefas {
                let _ = t.await;
            }
        }
        outro => anyhow::bail!("modo desconhecido: {outro}"),
    }
    Ok(())
}
