//! Confere que o binário de release não leva andaimes de medição lá dentro.
//!
//! # Porque é que isto existe
//!
//! Este projecto usa bandeiras de ambiente e comandos próprios para forçar caminhos que não
//! acontecem sozinhos — um estranho a injectar voz, um bloqueio a meio de uma sessão viva,
//! um convite fabricado com um caminho de ficheiro lá dentro. São necessários: um ataque que
//! não sai de casa passa em todos os testes.
//!
//! E não têm nada que ir no que se instala. Foram tirados à mão, e à mão ficaram **dois
//! comandos esquecidos** que só dei por eles a olhar para o exe publicado por outra razão.
//! Da próxima vez que acrescentar uma ferramenta de medição, o mesmo esquecimento chega
//! sozinho.
//!
//! Por isso é um portão e não uma boa intenção. Falha a publicação se algum destes nomes
//! aparecer no binário de release.
//!
//! # E porque é que uma busca de texto chega
//!
//! Não prova que o código foi removido — prova que o NOME não está lá. Mas o nome é
//! exactamente o que sobrevive: uma variável de ambiente é lida por nome, um comando do
//! Tauri é despachado por nome. Se o nome não está no binário, ninguém lhe chama. É a mesma
//! propriedade que interessa, e é verificável sem desmontar nada.
//!
//! O que isto NÃO apanha é um andaime que eu esqueça de acrescentar a esta lista. Por isso a
//! lista está aqui, ao lado do porquê, e não escondida num script.

use anyhow::{bail, Result};

/// Nomes que não podem existir num binário de release.
///
/// Cada um destes ou salta um guarda, ou escreve estado permanente, ou fabrica cargas de
/// ataque. Acrescentar um andaime novo é acrescentar uma linha aqui.
const ANDAIMES: &[&str] = &[
    "BRUMA_ESTRANHO",
    "BRUMA_ESTRANHO_SALA",
    "BRUMA_ESTRANHO_ACTO",
    "BRUMA_BLOQUEIA",
    "BRUMA_BLOQUEIA_TARDE",
    "BRUMA_AMIGO",
    "convites_de_teste",
    "escapou_alguma_coisa",
];

/// E estes TÊM de lá estar.
///
/// Sem isto, o portão passava por um binário vazio, truncado, ou compilado de um ramo
/// errado — e passava com um ar de aprovação. Um portão que só sabe dizer «não encontrei o
/// que não queria» não distingue «está limpo» de «não olhei para nada».
const TEM_DE_ESTAR: &[&str] = &["marcar_lido", "entrar_com_convite", "bruma"];

fn main() -> Result<()> {
    let Some(caminho) = std::env::args().nth(1) else {
        bail!("uso: so-o-que-vai-na-release <bruma.exe>");
    };
    let bytes = std::fs::read(&caminho)?;
    if bytes.len() < 1_000_000 {
        bail!(
            "o {caminho} tem só {} bytes — isso não é a app",
            bytes.len()
        );
    }

    let contem = |agulha: &str| bytes.windows(agulha.len()).any(|j| j == agulha.as_bytes());

    let em_falta: Vec<&str> = TEM_DE_ESTAR
        .iter()
        .copied()
        .filter(|a| !contem(a))
        .collect();
    if !em_falta.is_empty() {
        bail!(
            "o {caminho} não tem {em_falta:?} — ou não é o binário do Bruma, ou está \
             truncado. Não digo que está limpo sem primeiro saber que estou a olhar para a \
             coisa certa."
        );
    }

    let achados: Vec<&str> = ANDAIMES.iter().copied().filter(|a| contem(a)).collect();
    if !achados.is_empty() {
        bail!(
            "o {caminho} leva andaimes de medição lá dentro: {achados:?}\n\
             \n  Estes existem para medir e não para instalar. Põe-nos atrás de\n\
             `#[cfg(debug_assertions)]` — o ATRIBUTO, e não `cfg!()`: com `cfg!()` o\n\
             comportamento fica certo mas o nome continua no exe, e ja vi as duas metades do\n\
             mesmo padrão darem resultados diferentes no mesmo compilador."
        );
    }

    println!(
        "release limpa: {} bytes, nenhum dos {} andaimes, e os {} marcos estão lá",
        bytes.len(),
        ANDAIMES.len(),
        TEM_DE_ESTAR.len()
    );
    Ok(())
}
