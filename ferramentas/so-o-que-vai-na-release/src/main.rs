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
    // Esta faltava — e a prova de que a lista escrita de memória volta a falhar. Só não
    // escapou ao scan do binário porque `BRUMA_BLOQUEIA_TARDE` é prefixo dela e o `contem`
    // procura substrings; à verificação por NOME EXACTO das fontes (abaixo) já não passava.
    "BRUMA_BLOQUEIA_TARDE_MS",
    "BRUMA_AMIGO",
    // Partir a app de proposito, para o codigo que trata da avaria ser exercitado.
    // Estas nove escaparam a primeira versao desta lista -- eu escrevi-a de memoria em vez
    // de a tirar de um `grep`, que e exactamente o erro que esta ferramenta existe para
    // apanhar. Ver `ferramentas/so-o-que-vai-na-release` no README de commits.
    "BRUMA_SESSAO_MORRE",
    // A queda de uma sessao DE PE, e a substituicao de sessao forcada. As duas existem
    // porque a religacao -- o caso normal entre os EUA e o Brasil -- nao acontece nesta
    // maquina: sem elas, o contador de ligados, a marca «a religar» e o reenvio do
    // cabecalho ficavam por verificar. Andaimes de medicao, e o scan do binario garante
    // que nao vao na release.
    "BRUMA_SESSAO_MORRE_MS",
    "BRUMA_DISCAR_A_DOBRAR",
    // O atraso na escrita, que faz o canal de difusao transbordar. Sem ele o ramo do
    // `Lagged` -- onde as mensagens de TEXTO se perdiam em silencio -- nao e exercitado
    // por nada nesta maquina.
    "BRUMA_ESCRITA_LENTA_MS",
    "BRUMA_ESCRITA_LENTA_ATE_S",
    // O video por streams unidireccionais: e o passo 2 do #134, ainda por virar. Fica
    // andaime enquanto for bandeira; no dia em que virar, deixa de existir.
    "BRUMA_VIDEO_POR_UNI",
    // A morte da captura e a interface surda: os dois caminhos pelos quais uma partilha
    // acaba sem ninguem pedir, que nesta maquina nao acontecem sozinhos.
    "BRUMA_ITEM_FECHA_AOS",
    "BRUMA_UI_SURDA",
    "BRUMA_SYNC_LENTO",
    "BRUMA_SO_VIGIA",
    "BRUMA_SEM_TRAVAO",
    "BRUMA_CODIFICADOR_MORRE",
    "BRUMA_FALHA_CAPTURA",
    "BRUMA_SOM_MORRE",
    "BRUMA_SO_NOS",
    "BRUMA_ECO_ANTIGO",
    "BRUMA_SONDAGEM_RITMO",
    "BRUMA_SEM_CHAVE_A_PEDIDO",
    "BRUMA_SOM_NAO_VOLTA",
    "BRUMA_MOOF_CRU",
    "BRUMA_SOM_DEMORA_MS",
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

/* ===================================================================== o INSTALADOR

O `Instalar-Bruma.exe` é o ficheiro que toda a gente descarrega e o que o auto-update
corre — e nunca era inspeccionado por nada. O portão só olhava para o `bruma.exe`.

A app embutida vai comprimida lá dentro, portanto os nomes dela não aparecem aqui: o
que se procura são os andaimes DO INSTALADOR. Corre-se antes de o payload ser
embrulhado, pela mesma razão de sempre — depois de comprimido, uma busca de texto
deixaria de ver seja o que for e passaria sempre, sem provar nada.
*/

/// Andaimes que não podem ir no instalador publicado.
const ANDAIMES_DO_INSTALADOR: &[&str] = &[
    // O comando que faz a interface descrever-se a si própria. Sozinho é inócuo — imprime
    // uma linha —, mas a lista de comandos crescer sem ninguém olhar não é: foi assim que
    // dois comandos de medição chegaram a ir na release da app.
    "medir",
];

/// E o que TEM de lá estar, para não se dizer «limpo» sobre um ficheiro truncado.
const INSTALADOR_TEM_DE_ESTAR: &[&str] = &["uninstall.exe", "bruma.exe"];

/// O `--teste` FICA, e a razão tem de estar escrita.
///
/// É tentador metê-lo na lista de andaimes: é uma bandeira que existe para testar. Mas é ele
/// que torna este instalador verificável — o portão da release corre
/// `Instalar-Bruma.exe --silencioso --teste --dir=<pasta>` e prova a instalação e a
/// desinstalação inteiras sem UAC, sem registo e sem atalhos. Sem ele, o portão teria de
/// instalar no Program Files a sério, ou não existir.
///
/// E não é uma porta: em modo de teste o `sitios_dos_dados` protege o `%APPDATA%` de
/// propósito, portanto o caminho mais destrutivo que existe é o que ele NÃO alcança.
const INSTALADOR_LEGITIMO: &[&str] = &["--teste", "BRUMA_DADOS"];

/// Todos os nomes `BRUMA_*` que aparecem nas fontes.
///
/// Um scanner à mão e não uma regex: a única dependência desta ferramenta é o `anyhow`, e
/// assim fica. Apanha o nome onde quer que apareça — no `env::var`, num comentário, num
/// teste — porque um nome que aparece nas fontes é um nome que o portão tem de conhecer,
/// esteja onde estiver.
fn nomes_bruma_em(texto: &str) -> Vec<String> {
    let mut nomes = Vec::new();
    let bytes = texto.as_bytes();
    let agulha = b"BRUMA_";
    let mut i = 0;
    while let Some(j) = texto[i..].find("BRUMA_") {
        let inicio = i + j;
        let mut fim = inicio + agulha.len();
        while fim < bytes.len() && (bytes[fim].is_ascii_uppercase() || bytes[fim] == b'_') {
            fim += 1;
        }
        // `BRUMA_` sozinho (num comentário a falar do prefixo) não é um nome.
        if fim > inicio + agulha.len() {
            nomes.push(texto[inicio..fim].trim_end_matches('_').to_string());
        }
        i = fim;
    }
    nomes.sort();
    nomes.dedup();
    nomes
}

/// A verificação das FONTES: nenhum `BRUMA_*` pode existir no código sem estar classificado.
///
/// # A avaria que isto fecha
///
/// A própria ferramenta admite: «o que isto NÃO apanha é um andaime que eu esqueça de
/// acrescentar a esta lista». E aconteceu — nove nomes escaparam à primeira versão da
/// lista, escrita de memória, e o `BRUMA_BLOQUEIA_TARDE_MS` ficou de fora da segunda. Uma
/// lista escrita à mão diverge; uma lista CONFERIDA contra as fontes não consegue.
///
/// Cada nome encontrado tem de estar em `ANDAIMES` (e então o scan do binário garante que
/// não vai na release) ou numa das listas do que fica de propósito. Um nome novo em nenhuma
/// das duas é o portão a dizer: decide o que isto é antes de publicares.
fn verificar_fontes(raiz: &std::path::Path) -> Result<()> {
    let pastas = [
        "apps/desktop/src",
        "apps/instalador/src",
        "spikes/spike-common/src",
    ];
    let mut ficheiros = 0usize;
    let mut desconhecidos: Vec<(String, String)> = Vec::new();
    for pasta in pastas {
        let pasta = raiz.join(pasta);
        if !pasta.is_dir() {
            bail!(
                "não encontrei {} — corre isto da raiz do repositório",
                pasta.display()
            );
        }
        // RECURSIVO, e não um `read_dir` raso. Hoje não há um único .rs em subpastas destas
        // três árvores — mas no dia em que um módulo virar pasta (`rede/` em vez de
        // `rede.rs`), um andaime lá dentro escapava sem uma queixa. Uma verificação que só
        // cobre «o que existe hoje» é a lista de memória outra vez, com um passo a mais.
        let mut por_ver = vec![pasta];
        while let Some(d) = por_ver.pop() {
            for e in std::fs::read_dir(&d)?.flatten() {
                let caminho = e.path();
                if caminho.is_dir() {
                    por_ver.push(caminho);
                    continue;
                }
                if caminho.extension().is_none_or(|x| x != "rs") {
                    continue;
                }
                ficheiros += 1;
                let texto = std::fs::read_to_string(&caminho)?;
                for nome in nomes_bruma_em(&texto) {
                    let conhecido = ANDAIMES.contains(&nome.as_str())
                        || DIAGNOSTICO_LEGITIMO.contains(&nome.as_str())
                        || INSTALADOR_LEGITIMO.contains(&nome.as_str());
                    if !conhecido {
                        desconhecidos.push((nome, caminho.display().to_string()));
                    }
                }
            }
        }
    }
    if ficheiros == 0 {
        bail!("não li um único .rs — isto não verificou nada");
    }
    desconhecidos.sort();
    desconhecidos.dedup();
    if !desconhecidos.is_empty() {
        bail!(
            "as fontes usam nomes BRUMA_* que o portão não conhece: {desconhecidos:?}\n\
             \n  Cada um tem de entrar em ANDAIMES (se é andaime de medição, e aí o scan do\n\
             binário garante que não vai na release) ou na lista do que fica de propósito\n\
             (se é diagnóstico legítimo, com a razão escrita ao lado). Decidir é obrigatório;\n\
             deixar por classificar é como os nove primeiros escaparam."
        );
    }
    println!("fontes limpas: {ficheiros} ficheiros lidos, todos os nomes BRUMA_* classificados");
    Ok(())
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let Some(caminho) = args.next() else {
        bail!("uso: so-o-que-vai-na-release <exe> [--instalador] | --fontes <raiz>");
    };
    if caminho == "--fontes" {
        let raiz = args.next().unwrap_or_else(|| ".".into());
        return verificar_fontes(std::path::Path::new(&raiz));
    }
    let e_instalador = args.any(|a| a == "--instalador");
    let (andaimes, tem_de_estar, legitimo, quem) = if e_instalador {
        (
            ANDAIMES_DO_INSTALADOR,
            INSTALADOR_TEM_DE_ESTAR,
            INSTALADOR_LEGITIMO,
            "instalador",
        )
    } else {
        (ANDAIMES, TEM_DE_ESTAR, DIAGNOSTICO_LEGITIMO, "app")
    };
    let bytes = std::fs::read(&caminho)?;
    if bytes.len() < 1_000_000 {
        bail!(
            "o {caminho} tem só {} bytes — isso não é a app",
            bytes.len()
        );
    }

    let contem = |agulha: &str| bytes.windows(agulha.len()).any(|j| j == agulha.as_bytes());

    let em_falta: Vec<&str> = tem_de_estar
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

    let achados: Vec<&str> = andaimes.iter().copied().filter(|a| contem(a)).collect();
    if !achados.is_empty() {
        bail!(
            "o {caminho} leva andaimes de medição lá dentro: {achados:?}\n\
             \n  Estes existem para medir e não para instalar. Põe-nos atrás de\n\
             `#[cfg(debug_assertions)]` — o ATRIBUTO, e não `cfg!()`: com `cfg!()` o\n\
             comportamento fica certo mas o nome continua no exe, e ja vi as duas metades do\n\
             mesmo padrão darem resultados diferentes no mesmo compilador."
        );
    }

    let diagnostico: Vec<&str> = legitimo.iter().copied().filter(|a| contem(a)).collect();
    println!(
        "release limpa ({quem}): {} bytes | {} andaimes procurados, nenhum encontrado | {} marcos presentes | o que fica de propósito: {:?}",
        bytes.len(),
        andaimes.len(),
        tem_de_estar.len(),
        diagnostico
    );
    Ok(())
}

#[cfg(test)]
mod testes {
    use super::*;

    /// O scanner apanha o nome inteiro, e não o prefixo de outro.
    ///
    /// Foi exactamente assim que o `BRUMA_BLOQUEIA_TARDE_MS` ficou de fora da lista: o scan
    /// do binário procura substrings e o prefixo tapava-o. A verificação das fontes compara
    /// nomes EXACTOS, portanto o scanner tem de os extrair exactos.
    #[test]
    fn o_scanner_extrai_nomes_inteiros_e_deduplicados() {
        let texto = r#"
            std::env::var("BRUMA_BLOQUEIA_TARDE_MS").ok();
            // fala-se do BRUMA_ESTRANHO num comentário
            var("BRUMA_ESTRANHO"); var("BRUMA_ESTRANHO");
        "#;
        assert_eq!(
            nomes_bruma_em(texto),
            vec!["BRUMA_BLOQUEIA_TARDE_MS", "BRUMA_ESTRANHO"]
        );
    }

    /// O prefixo sozinho — «as variáveis BRUMA_ …» — não é um nome.
    #[test]
    fn o_prefixo_sozinho_nao_conta() {
        assert!(nomes_bruma_em("as variáveis BRUMA_ são de medição").is_empty());
    }

    /// E o scan DESCE a subpastas: um módulo que vire pasta não abre um buraco.
    #[test]
    fn o_scan_e_recursivo() {
        let raiz = std::env::temp_dir().join(format!("bruma-fontes-{}", std::process::id()));
        let fundo = raiz.join("apps/desktop/src/sub/mais");
        std::fs::create_dir_all(&fundo).unwrap();
        std::fs::create_dir_all(raiz.join("apps/instalador/src")).unwrap();
        std::fs::create_dir_all(raiz.join("spikes/spike-common/src")).unwrap();
        std::fs::write(fundo.join("x.rs"), "var(\"BRUMA_ESCONDIDA\")").unwrap();
        let erro = verificar_fontes(&raiz).expect_err("o nome na subpasta tinha de ser visto");
        assert!(
            erro.to_string().contains("BRUMA_ESCONDIDA"),
            "a queixa tem de dizer o nome: {erro}"
        );
        let _ = std::fs::remove_dir_all(&raiz);
    }

    /// E as fontes REAIS deste repositório passam — com a lista de hoje.
    ///
    /// Corre sobre o código verdadeiro: se alguém acrescentar um `BRUMA_QUALQUER` sem o
    /// classificar, isto falha no `cargo test` antes sequer de chegar ao portão da release.
    #[test]
    fn as_fontes_reais_estao_classificadas() {
        let raiz = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        verificar_fontes(&raiz).expect("um nome BRUMA_* por classificar");
    }
}
