//! Lê o registo de uma corrida de medição e reprova, com código != 0, se alguma coisa correu
//! mal — ou se não correu de todo.
//!
//! # Porque é que isto existe
//!
//! Este projecto mede tudo. O `--medir-ui` imprime 125 linhas sobre a interface, o `--par`
//! outras 35 sobre duas máquinas a falar, o `--autoteste` outras tantas sobre a partilha de
//! ecrã. E até hoje **nenhuma delas reprovava**: a saída ia para o `bruma.log` e a única coisa
//! que a lia eram os olhos do dono. `par CONVIDADO recebeu 3/5 mensagens` imprimia-se, e a
//! corrida dava-se por boa na mesma.
//!
//! A expectativa não estava no registo — estava na cabeça de quem o lia. Este portão existe
//! porque essa expectativa passou a estar escrita: cada medição declara o seu veredicto na
//! gramática do `contadorDeMedidas` (apps/desktop/ui/app.js), e aqui julga-se o que ela disse.
//!
//! # A gramática
//!
//! ```text
//! MEDICAO <guião> INICIO
//! MEDIDA <nome> ok (<detalhe>)
//! MEDIDA <nome> mau (<detalhe>)
//! MEDIDA <nome> NAO CORRI (<razão>)
//! MEDICAO <guião> FIM ok=<n> mau=<n> nao-corri=<n>
//! ```
//!
//! As linhas de facto que já existiam — `ui stream incremental: msgs=61 …` — ficam como
//! estão. São a PROVA, e é nelas que se percebe o que aconteceu; o veredicto vai ao lado.
//!
//! # E porque é que um portão precisa de saber o que TEM de estar lá
//!
//! Um portão que só sabe procurar o que é mau dá verde a um registo vazio, a um guião que
//! morreu na terceira linha, e a uma medição que alguém apagou sem dar por isso. É o mesmo
//! defeito que o `so-o-que-vai-na-release` já tinha corrigido com a sua lista `TEM_DE_ESTAR`.
//! Por isso este portão tem um MANIFESTO: os nomes que cada guião tem obrigatoriamente de
//! declarar. Apagar uma medição passa a ser uma decisão que se toma aqui, à vista.

use anyhow::{bail, Result};

/// O que cada guião TEM de medir. Sem isto, «passou» quer dizer «não encontrei nada de mau»,
/// que é compatível com não ter medido nada.
///
/// Acrescentar um nome aqui é dizer «esta medição passou a fazer parte do que se exige».
/// Tirar um é uma decisão deliberada, e fica no diff.
const OBRIGATORIAS: &[(&str, &[&str])] = &[
    (
        "ui",
        &[
            "foco-da-por-lido",
            "dois-cliques-o-segundo-ganha",
            "stream-incremental",
            "uma-rajada-um-redesenho",
            "entrega-por-confirmar",
            "so-se-abrem-enderecos-http",
            "numero-de-seguranca-nao-e-a-semente",
            "canal-reaberto-limpa-a-faixa",
            "o-x-esconde-e-nao-mata",
            "aviso-clicado-abre-o-destino",
            "arranque-nao-decifra-nada",
        ],
    ),
    ("par-anfitriao", &["escreveu-antes-de-haver-convidado"]),
    (
        "par-convidado",
        &[
            "recebeu-o-que-se-escreveu-sem-ele",
            "recebeu-o-que-veio-depois-do-atraso",
            "abrir-a-conversa-poe-o-por-ler-a-zero",
        ],
    ),
];

/// Medições que podem não correr sem que isso reprove, com a razão escrita ao lado.
///
/// É a válvula, e é estreita de propósito: uma dispensa é uma promessa de que aquilo não se
/// consegue medir aqui, não um sítio para arrumar uma medição incómoda.
const DISPENSADAS: &[(&str, &str)] = &[
    // O eco precisa de placa de som e de colunas activas; num runner não há nem uma nem
    // outra, e forçar o caminho sem elas mediria o silêncio.
    (
        "eco",
        "precisa de placa de som e colunas — corre-se em casa",
    ),
];

/// Palavras que, em qualquer linha de medição, querem dizer que alguma coisa se partiu.
const PALAVRAS_MAS: &[&str] = &[
    "REBENTOU",
    "FALHOU",
    "NAO EXISTE",
    "NAO ABRIU",
    "JS ERRO",
    "JS PROMESSA REJEITADA",
    "nunca chegou",
];

/// Um veredicto lido do registo.
#[derive(Debug, PartialEq)]
enum Veredicto {
    Ok,
    Mau(String),
    NaoCorri(String),
}

#[derive(Debug, Default)]
struct Guiao {
    comecou: bool,
    acabou: bool,
    fim_ok: usize,
    medidas: Vec<(String, Veredicto)>,
}

/// Descasca o que o registo põe à frente: `[HH:MM:SS]` do `registo.rs` e `  capacidades: ` do
/// comando que a interface usa para falar. Uma corrida feita de um terminal não leva carimbo
/// nenhum — por isso os dois prefixos são opcionais.
fn descascar(linha: &str) -> &str {
    let l = linha.trim_start();
    let l = match l.strip_prefix('[') {
        Some(resto) => resto
            .split_once(']')
            .map(|(_, r)| r)
            .unwrap_or(l)
            .trim_start(),
        None => l,
    };
    l.strip_prefix("capacidades:").unwrap_or(l).trim()
}

fn ler(texto: &str) -> Result<(std::collections::BTreeMap<String, Guiao>, Vec<String>)> {
    let mut guioes: std::collections::BTreeMap<String, Guiao> = Default::default();
    let mut partidas: Vec<String> = Vec::new();
    let mut actual: Option<String> = None;

    for bruta in texto.lines() {
        let linha = descascar(bruta);

        if let Some(resto) = linha.strip_prefix("MEDICAO ") {
            let mut p = resto.split_whitespace();
            let nome = p.next().unwrap_or_default().to_string();
            match p.next() {
                Some("INICIO") => {
                    guioes.entry(nome.clone()).or_default().comecou = true;
                    actual = Some(nome);
                }
                Some("FIM") => {
                    let g = guioes.entry(nome.clone()).or_default();
                    g.acabou = true;
                    for campo in p {
                        if let Some(v) = campo.strip_prefix("ok=") {
                            g.fim_ok = v.parse().unwrap_or(0);
                        }
                    }
                }
                _ => {}
            }
            continue;
        }

        if let Some(resto) = linha.strip_prefix("MEDIDA ") {
            let (nome, cauda) = resto.split_once(' ').unwrap_or((resto, ""));
            let detalhe = cauda
                .split_once('(')
                .and_then(|(_, d)| d.rsplit_once(')'))
                .map(|(d, _)| d.to_string())
                .unwrap_or_default();
            let v = if cauda.starts_with("ok") {
                Veredicto::Ok
            } else if cauda.starts_with("mau") {
                Veredicto::Mau(detalhe)
            } else if cauda.starts_with("NAO CORRI") {
                Veredicto::NaoCorri(detalhe)
            } else {
                partidas.push(format!("veredicto que não percebo: {linha}"));
                continue;
            };
            let g = match &actual {
                Some(n) => guioes.entry(n.clone()).or_default(),
                // Um `MEDIDA` antes de qualquer `MEDICAO … INICIO` é registo de outra corrida
                // colado ao mesmo ficheiro, ou uma gramática mal usada. Conta como partida.
                None => {
                    partidas.push(format!("MEDIDA fora de um guião: {linha}"));
                    continue;
                }
            };
            g.medidas.push((nome.to_string(), v));
            continue;
        }

        // As palavras más só contam em linhas DE MEDIÇÃO — senão um `[rede] … FALHOU` de um
        // caminho que o próprio guião provoca de propósito reprovava a corrida inteira.
        let de_medicao = linha.starts_with("ui ")
            || linha.starts_with("par ")
            || linha.starts_with("autoteste ")
            || linha.starts_with("JS ");
        if de_medicao {
            if let Some(p) = PALAVRAS_MAS.iter().find(|p| linha.contains(**p)) {
                partidas.push(format!("«{p}» em: {linha}"));
            }
        }
    }
    Ok((guioes, partidas))
}

fn julgar(
    guioes: &std::collections::BTreeMap<String, Guiao>,
    partidas: &[String],
    exigidos: &[String],
) -> Result<String> {
    let mut queixas: Vec<String> = partidas.to_vec();

    for nome in exigidos {
        if !guioes.contains_key(nome) {
            queixas.push(format!(
                "o guião «{nome}» não correu de todo: não há nenhuma linha `MEDICAO {nome} INICIO`"
            ));
        }
    }

    for (nome, g) in guioes {
        if g.comecou && !g.acabou {
            queixas.push(format!(
                "o guião «{nome}» começou e não chegou ao fim ({} medições lidas): o registo está \
                 truncado, ou o guião morreu a meio",
                g.medidas.len()
            ));
        }
        if g.acabou && g.fim_ok == 0 {
            queixas.push(format!("o guião «{nome}» acabou sem uma única medição boa"));
        }
        for (medida, v) in &g.medidas {
            match v {
                Veredicto::Ok => {}
                Veredicto::Mau(d) => queixas.push(format!("{nome}/{medida}: mau ({d})")),
                Veredicto::NaoCorri(razao) => {
                    match DISPENSADAS.iter().find(|(n, _)| *n == medida) {
                        Some(_) => {}
                        None => queixas.push(format!("{nome}/{medida}: NÃO CORREU ({razao})")),
                    }
                }
            }
        }
        // O mesmo nome com dois veredictos deixa o portão sem resposta: reprova.
        for (medida, _) in &g.medidas {
            let quantos = g.medidas.iter().filter(|(m, _)| m == medida).count();
            let distintos = g
                .medidas
                .iter()
                .filter(|(m, _)| m == medida)
                .filter(|(_, v)| matches!(v, Veredicto::Ok))
                .count();
            if quantos > 1 && distintos != quantos && distintos != 0 {
                queixas.push(format!(
                    "{nome}/{medida}: declarado {quantos} vezes com veredictos diferentes"
                ));
            }
        }
        // E o manifesto: o que TEM de estar lá.
        if let Some((_, obrigatorias)) = OBRIGATORIAS.iter().find(|(n, _)| n == nome) {
            for tem in *obrigatorias {
                if !g.medidas.iter().any(|(m, _)| m == tem) {
                    queixas.push(format!(
                        "{nome}/{tem}: está no manifesto e não foi declarada nesta corrida"
                    ));
                }
            }
        }
    }

    if !queixas.is_empty() {
        bail!(
            "{} coisa(s) por explicar:\n  - {}",
            queixas.len(),
            queixas.join("\n  - ")
        );
    }

    let resumo = guioes
        .iter()
        .map(|(n, g)| {
            let ok = g
                .medidas
                .iter()
                .filter(|(_, v)| *v == Veredicto::Ok)
                .count();
            format!("{n}: {ok} ok")
        })
        .collect::<Vec<_>>()
        .join(" | ");
    Ok(if resumo.is_empty() {
        "nenhum guião no registo".into()
    } else {
        resumo
    })
}

fn main() -> Result<()> {
    let mut ficheiros: Vec<String> = Vec::new();
    let mut exigidos: Vec<String> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--guiao" => exigidos.push(args.next().unwrap_or_default()),
            _ => ficheiros.push(a),
        }
    }
    if ficheiros.is_empty() {
        bail!("uso: o-que-falhou <registo.log> [outro.log …] [--guiao ui] [--guiao par-anfitriao]");
    }

    let mut texto = String::new();
    for f in &ficheiros {
        let lido =
            std::fs::read_to_string(f).map_err(|e| anyhow::anyhow!("não consegui ler {f}: {e}"))?;
        texto.push_str(&lido);
        texto.push('\n');
    }
    let (guioes, partidas) = ler(&texto)?;
    match julgar(&guioes, &partidas, &exigidos) {
        Ok(resumo) => {
            println!("medições em ordem — {resumo}");
            Ok(())
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    fn julga(texto: &str, exigidos: &[&str]) -> Result<String> {
        let (g, p) = ler(texto).unwrap();
        let exigidos: Vec<String> = exigidos.iter().map(|s| s.to_string()).collect();
        julgar(&g, &p, &exigidos)
    }

    /// O caso bom, com os dois prefixos que o registo põe à frente.
    #[test]
    fn uma_corrida_boa_passa() {
        let log = "\
[10:00:00]   capacidades: MEDICAO x INICIO
[10:00:01]   capacidades: ui alguma coisa: campo=1
[10:00:02]   capacidades: MEDIDA uma ok (detalhe)
[10:00:03]   capacidades: MEDICAO x FIM ok=1 mau=0 nao-corri=0
";
        assert!(julga(log, &["x"]).is_ok());
    }

    /// E sem carimbo nenhum, que é como sai quando se corre de um terminal.
    #[test]
    fn sem_carimbo_tambem_se_le() {
        let log = "MEDICAO x INICIO\nMEDIDA uma ok ()\nMEDICAO x FIM ok=1 mau=0 nao-corri=0\n";
        assert!(julga(log, &[]).is_ok());
    }

    #[test]
    fn uma_medida_ma_reprova() {
        let log = "MEDICAO x INICIO\nMEDIDA uma mau (deu 3, esperava 5)\nMEDICAO x FIM ok=0 mau=1 nao-corri=0\n";
        let e = julga(log, &[]).unwrap_err().to_string();
        assert!(e.contains("x/uma: mau (deu 3, esperava 5)"), "{e}");
    }

    /// O caso que o portão existe para apanhar: o guião morreu a meio e o registo acaba.
    #[test]
    fn um_guiao_truncado_reprova() {
        let log = "MEDICAO x INICIO\nMEDIDA uma ok ()\n";
        let e = julga(log, &[]).unwrap_err().to_string();
        assert!(e.contains("não chegou ao fim"), "{e}");
    }

    /// E o guião que nunca começou: um registo vazio não pode passar por aprovação.
    #[test]
    fn um_guiao_que_nao_correu_reprova() {
        let e = julga("[10:00:00] o Bruma arrancou\n", &["ui"])
            .unwrap_err()
            .to_string();
        assert!(e.contains("não correu de todo"), "{e}");
    }

    #[test]
    fn acabar_sem_uma_medicao_boa_reprova() {
        let log = "MEDICAO x INICIO\nMEDICAO x FIM ok=0 mau=0 nao-corri=0\n";
        let e = julga(log, &[]).unwrap_err().to_string();
        assert!(e.contains("sem uma única medição boa"), "{e}");
    }

    #[test]
    fn uma_medicao_que_nao_correu_reprova_salvo_dispensa() {
        let log = "MEDICAO x INICIO\nMEDIDA uma ok ()\nMEDIDA outra NAO CORRI (sem servidor)\nMEDICAO x FIM ok=1 mau=0 nao-corri=1\n";
        let e = julga(log, &[]).unwrap_err().to_string();
        assert!(e.contains("x/outra: NÃO CORREU (sem servidor)"), "{e}");

        let dispensada = log.replace("outra", DISPENSADAS[0].0);
        assert!(julga(&dispensada, &[]).is_ok(), "a dispensa tem de valer");
    }

    #[test]
    fn as_palavras_mas_reprovam_so_em_linhas_de_medicao() {
        let log = "MEDICAO x INICIO\nMEDIDA uma ok ()\nui seletor: NAO ABRIU\nMEDICAO x FIM ok=1 mau=0 nao-corri=0\n";
        assert!(julga(log, &[]).is_err());
        // Uma linha do Rust que fala de uma falha que o próprio guião provoca não conta.
        let outro = "MEDICAO x INICIO\nMEDIDA uma ok ()\n[rede] a sessão FALHOU (bandeira de teste)\nMEDICAO x FIM ok=1 mau=0 nao-corri=0\n";
        assert!(julga(outro, &[]).is_ok());
    }

    #[test]
    fn o_manifesto_apanha_uma_medicao_que_desapareceu() {
        let obrigatoria = OBRIGATORIAS
            .iter()
            .find(|(n, _)| *n == "ui")
            .map(|(_, m)| m[0])
            .unwrap();
        let log = "MEDICAO ui INICIO\nMEDIDA qualquer-outra ok ()\nMEDICAO ui FIM ok=1 mau=0 nao-corri=0\n";
        let e = julga(log, &["ui"]).unwrap_err().to_string();
        assert!(e.contains(obrigatoria), "{e}");
        assert!(e.contains("está no manifesto"), "{e}");
    }

    /// Dois logs (o anfitrião e o convidado) julgam-se juntos, como o `--par` os produz.
    #[test]
    fn dois_guioes_no_mesmo_julgamento() {
        let log = "MEDICAO par-anfitriao INICIO\nMEDIDA a ok ()\nMEDICAO par-anfitriao FIM ok=1 mau=0 nao-corri=0\n\
MEDICAO par-convidado INICIO\nMEDIDA b ok ()\nMEDICAO par-convidado FIM ok=1 mau=0 nao-corri=0\n";
        let (g, _) = ler(log).unwrap();
        assert_eq!(g.len(), 2);
    }
}
