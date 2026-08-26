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
    // Simular um estranho, bloquear a meio, forjar amizade.
    "BRUMA_ESTRANHO",
    "BRUMA_ESTRANHO_SALA",
    "BRUMA_ESTRANHO_ACTO",
    "BRUMA_BLOQUEIA",
    "BRUMA_BLOQUEIA_TARDE",
    "BRUMA_AMIGO",
    // Partir a app de proposito, para o codigo que trata da avaria ser exercitado.
    // Estas nove escaparam a primeira versao desta lista -- eu escrevi-a de memoria em vez
    // de a tirar de um `grep`, que e exactamente o erro que esta ferramenta existe para
    // apanhar. Ver `ferramentas/so-o-que-vai-na-release` no README de commits.
    "BRUMA_SESSAO_MORRE",
    "BRUMA_SYNC_LENTO",
    "BRUMA_SO_VIGIA",
    "BRUMA_SEM_TRAVAO",
    "BRUMA_CODIFICADOR_MORRE",
    "BRUMA_FALHA_CAPTURA",
    "BRUMA_SOM_MORRE",
    "BRUMA_SO_NOS",
    "BRUMA_ECO_ANTIGO",
    // Comandos que so servem para medir.
    "convites_de_teste",
    "escapou_alguma_coisa",
    "autoteste_pedido",
    "autoteste_par",
    "medir_ui_pedido",
];

/// As que FICAM, e porque.
///
/// `BRUMA_DADOS` escolhe a pasta de dados e `BRUMA_REGISTO` o nivel de registo. Sao
/// diagnostico legitimo, estao documentados no README, e servem a quem instalou a app quando
/// alguma coisa corre mal. Estao aqui para a distincao ficar escrita e nao ter de ser
/// relembrada: a pergunta nao e "e uma variavel de ambiente?" mas "isto parte alguma coisa ou
/// escreve estado que a pessoa nao pediu?".
const DIAGNOSTICO_LEGITIMO: &[&str] = &["BRUMA_DADOS", "BRUMA_REGISTO"];

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

    let diagnostico: Vec<&str> = DIAGNOSTICO_LEGITIMO
        .iter()
        .copied()
        .filter(|a| contem(a))
        .collect();
    println!(
        "release limpa: {} bytes | {} andaimes procurados, nenhum encontrado | {} marcos presentes | diagnóstico que fica: {:?}",
        bytes.len(),
        ANDAIMES.len(),
        TEM_DE_ESTAR.len(),
        diagnostico
    );
    Ok(())
}
