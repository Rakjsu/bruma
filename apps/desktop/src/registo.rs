//! Um ficheiro de registo, para que o que a app diz não se perca.
//!
//! # A avaria que isto corrige
//!
//! O binário de release é compilado com `windows_subsystem = "windows"` — **não tem
//! consola**. Todos os `println!` e `eprintln!` do projeto escrevem para um sítio que não
//! existe, e portanto não escrevem para lado nenhum. Isso inclui o canal de diagnóstico
//! inteiro: o `--autoteste`, o `--medir-ui` e cada `diz(...)` da interface passam pelo
//! comando `capacidades`, que é um `println!`. **A ferramenta de diagnóstico não funcionava
//! precisamente no build onde faz falta.**
//!
//! Na prática: quando alguma coisa corria mal em casa de outra pessoa, não havia nada para
//! ler. Nem o erro, nem sequer a certeza de que a app tinha chegado a arrancar.
//!
//! # Porquê trocar o descritor em vez de mudar as 39 chamadas
//!
//! A alternativa era passar cada `eprintln!` por uma macro nossa. São 39 sítios espalhados
//! por seis ficheiros, e cada um seria uma oportunidade para esquecer um — e o esquecido
//! seria, pela lei do costume, o do caminho que mais interessa. Trocar o descritor do
//! processo apanha **todos**, incluindo os que ainda não foram escritos, e os que vêm de
//! dentro de bibliotecas.
//!
//! O `SetStdHandle` tem de acontecer **antes** de o Rust tocar no stdout: o `std` guarda o
//! descritor na primeira utilização e não volta a perguntar. Por isso isto é a primeira
//! coisa que o `main` faz.

use std::path::PathBuf;

/// Onde o registo vive, ao lado dos dados.
pub fn caminho() -> PathBuf {
    crate::estado::raiz().join("bruma.log")
}

/// Acima disto, o registo passa a `bruma.log.antigo` e recomeça. Dois ficheiros bastam:
/// o de agora e o da sessão que correu mal antes desta.
const MAXIMO: u64 = 4 * 1024 * 1024;

/// Encaminha o que a app escreve para um ficheiro, e apanha os pânicos.
///
/// Chamar UMA vez, no princípio do `main`. Falhar aqui nunca impede a app de arrancar —
/// ficar sem registo é mau, não abrir é pior.
pub fn arrancar() {
    let destino = caminho();
    if let Some(pai) = destino.parent() {
        let _ = std::fs::create_dir_all(pai);
    }
    if std::fs::metadata(&destino).map(|m| m.len()).unwrap_or(0) > MAXIMO {
        let _ = std::fs::rename(&destino, destino.with_extension("log.antigo"));
    }

    let aberto = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&destino);
    let Ok(ficheiro) = aberto else {
        return;
    };

    // Só se rouba a saída a quem NÃO a tem. Lançada do Explorador — o caso normal em
    // release — não há consola nenhuma e tudo se perderia; lançada de um terminal, os
    // descritores foram herdados e a pessoa está à espera de ver ali o que a app diz.
    // Tirar-lhe isso para dentro de um ficheiro seria trocar um silêncio por outro.
    #[cfg(windows)]
    if sem_consola() || std::env::var("BRUMA_REGISTO").is_ok() {
        // Por VALOR: o ficheiro tem de sobreviver a esta função. Ver `encaminhar`.
        encaminhar(ficheiro);
    }

    // O pânico é o caso em que mais faz falta haver registo — é literalmente o único
    // vestígio que fica de uma app que abriu, piscou e desapareceu. Escreve-se DIRECTO ao
    // ficheiro, e não pelo `eprintln!`: quando há consola o `eprintln!` vai para lá, e é
    // precisamente do pânico que se quer cópia guardada em qualquer dos casos.
    let para_o_panico = destino.clone();
    let anterior = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        anotar(&para_o_panico, &format!("[pânico] {info}"));
        anterior(info);
    }));

    anotar(
        &destino,
        &format!("\n===== Bruma {} arrancou =====", env!("CARGO_PKG_VERSION")),
    );
}

/// Escreve uma linha directamente no ficheiro, sem passar pelos descritores.
fn anotar(destino: &std::path::Path, linha: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(destino)
    {
        let _ = writeln!(f, "{linha}");
    }
}

/// Se este processo tem para onde escrever. Um handle nulo ou inválido significa que não.
#[cfg(windows)]
fn sem_consola() -> bool {
    use windows::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows::Win32::System::Console::{GetStdHandle, STD_OUTPUT_HANDLE};
    unsafe {
        match GetStdHandle(STD_OUTPUT_HANDLE) {
            Ok(h) => h.is_invalid() || h == INVALID_HANDLE_VALUE,
            Err(_) => true,
        }
    }
}

/// Troca os descritores de saída do processo pelo ficheiro.
///
/// Mexe-se nos descritores do PROCESSO e não só nos do Rust, porque o `windows-capture`, o
/// `iroh` e o próprio WebView2 também escrevem para lá quando se queixam — e essas queixas
/// são exactamente as que não se conseguiam ler.
#[cfg(windows)]
fn encaminhar(ficheiro: std::fs::File) {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Console::{SetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE};

    let h = HANDLE(ficheiro.as_raw_handle());
    unsafe {
        let _ = SetStdHandle(STD_OUTPUT_HANDLE, h);
        let _ = SetStdHandle(STD_ERROR_HANDLE, h);
    }
    // `forget` do PRÓPRIO ficheiro, e é deliberado: os descritores do processo apontam
    // para ele durante toda a vida da app. Da primeira vez esqueci-me de um CLONE e deixei
    // o original ser destruído no fim da função — o handle fechava, os descritores ficavam
    // a apontar para nada, e o registo escrevia para o vazio. O cabeçalho aparecia à mesma
    // (esse vai directo ao ficheiro) e portanto parecia estar tudo bem.
    std::mem::forget(ficheiro);
}
