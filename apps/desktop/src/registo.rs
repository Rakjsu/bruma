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

/// Troca os descritores de saída do processo por um CANO, e carimba o que sai dele (#143).
///
/// Mexe-se nos descritores do PROCESSO e não só nos do Rust, porque o `windows-capture`, o
/// `iroh` e o próprio WebView2 também escrevem para lá quando se queixam — e essas queixas
/// são exactamente as que não se conseguiam ler.
///
/// # Porque é que agora há um cano pelo meio
///
/// O registo não tinha horas. E as linhas que mais interessam neste projecto são todas
/// sobre TEMPO: «religado a X», «X não atendeu; nova tentativa daqui a N s», «X atrasou-se e
/// perdeu N pedaços», «a rede mudou». Sem hora, saber que a ligação caiu não diz se caiu
/// há dez segundos ou há duas horas — e é essa a pergunta.
///
/// Carimbar no `eprintln!` não servia: metade das linhas vem de dentro de bibliotecas, que
/// é precisamente o motivo por que se trocam os descritores do processo. Por isso o handle
/// que vai para o `SetStdHandle` passa a ser a ponta de escrita de um cano anónimo, e uma
/// linha de execução lê a outra ponta, parte por `\n` e escreve no ficheiro com a hora à
/// frente.
///
/// # Os dois cuidados que isto obriga
///
/// **Escritas parciais.** Uma biblioteca pode escrever meia linha e o resto a seguir. O
/// leitor guarda o que sobra num tampão e só carimba quando aparece o `\n` — senão o
/// carimbo aparecia a meio de uma frase.
///
/// **A linha que nunca acaba.** Se alguém escrever muito sem um `\n`, o tampão cresceria
/// sem fim. Ao fim de 64 KiB escreve-se o que há, com uma marca a dizer que foi cortado.
#[cfg(windows)]
fn encaminhar(ficheiro: std::fs::File) {
    use std::io::{Read, Write};
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Console::{SetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE};
    use windows::Win32::System::Pipes::CreatePipe;

    let mut ler = windows::Win32::Foundation::HANDLE::default();
    let mut escrever = windows::Win32::Foundation::HANDLE::default();
    let feito = unsafe { CreatePipe(&mut ler, &mut escrever, None, 0) };
    if feito.is_err() {
        // Sem cano não há carimbo, mas o registo continua a existir: vale mais um ficheiro
        // sem horas do que nenhum. É o mesmo princípio de toda esta função — falhar aqui
        // nunca impede a app de arrancar.
        let h = HANDLE(ficheiro.as_raw_handle());
        unsafe {
            let _ = SetStdHandle(STD_OUTPUT_HANDLE, h);
            let _ = SetStdHandle(STD_ERROR_HANDLE, h);
        }
        std::mem::forget(ficheiro);
        return;
    }

    unsafe {
        let _ = SetStdHandle(STD_OUTPUT_HANDLE, escrever);
        let _ = SetStdHandle(STD_ERROR_HANDLE, escrever);
    }

    // O `HANDLE` do `windows` é um `*mut c_void`, que não é `Send`. Converte-se JÁ AQUI
    // para um `File` — que é `Send` — em vez de o atravessar em cru: o handle passa a ser
    // dono de si próprio antes de mudar de linha de execução, e é o `File` que viaja.
    let entrada = unsafe { std::fs::File::from_raw_handle(ler.0 as *mut _) };

    // A ponta de leitura e o ficheiro vivem nesta linha de execução, e ela vive para
    // sempre. O `ficheiro` entra por valor precisamente por isso.
    std::thread::spawn(move || {
        let mut entrada = entrada;
        let mut saida = ficheiro;
        let mut sobra: Vec<u8> = Vec::new();
        let mut bloco = [0u8; 8192];
        loop {
            let n = match entrada.read(&mut bloco) {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            };
            sobra.extend_from_slice(&bloco[..n]);
            while let Some(fim) = sobra.iter().position(|b| *b == b'\n') {
                let linha: Vec<u8> = sobra.drain(..=fim).collect();
                let texto = String::from_utf8_lossy(&linha[..linha.len() - 1]);
                let texto = texto.trim_end_matches('\r');
                let _ = writeln!(saida, "{} {}", agora_curto(), texto);
            }
            // A linha que nunca acaba: despeja-se o que há e diz-se que foi cortada.
            if sobra.len() > 64 * 1024 {
                let texto = String::from_utf8_lossy(&sobra);
                let _ = writeln!(
                    saida,
                    "{} {texto} [linha cortada aos 64 KiB]",
                    agora_curto()
                );
                sobra.clear();
            }
            let _ = saida.flush();
        }
    });
}

/// A hora LOCAL, curta, para carimbar cada linha do registo.
///
/// Sem dependências novas: o `chrono` não está aqui e não vale a pena trazê-lo por isto. A
/// data completa também não vale — o ficheiro roda a cada 4 MiB e o cabeçalho de arranque
/// diz quando a sessão começou; o que falta a cada linha é a hora dentro dessa sessão.
///
/// # Porque é que não é o relógio de UTC
///
/// A primeira versão disto contava segundos desde a época e fazia as contas à mão. Dava
/// UTC, e o comentário dizia «hora local» — uma diferença de três a oito horas para as duas
/// pessoas que usam isto. Quem lê um registo compara-o com o que se lembra de ter
/// acontecido, e uma hora que não bate com o relógio da parede é pior do que nenhuma:
/// manda procurar no sítio errado.
#[cfg(windows)]
fn agora_curto() -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!("[{:02}:{:02}:{:02}]", t.wHour, t.wMinute, t.wSecond)
}
