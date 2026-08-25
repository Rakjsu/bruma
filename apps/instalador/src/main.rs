//! O instalador do Bruma, escrito de raiz.
//!
//! # Porquê um instalador nosso
//!
//! O que se descarregava era o assistente do NSIS — funcional, mas genérico, e vestir-lhe
//! imagens não o tornava nosso. Este é uma aplicação: a mesma pilha da app (Tauri), a
//! mesma paleta, o mesmo identicon, o mesmo tom. Transporta o `bruma.exe` comprimido
//! dentro de si (ver `build.rs`), portanto instala offline e não descarrega nada.
//!
//! O NSIS não desapareceu — ficou onde é invisível: o canal do auto-update, que descarrega
//! e corre instaladores em silêncio e cuja assinatura é gerada sobre o artefacto do
//! bundler. O que uma pessoa vê é isto; o que uma máquina corre às escondidas continua a
//! ser o canal que já estava provado.
//!
//! # O mesmo executável é o desinstalador
//!
//! Na instalação, uma cópia deste exe fica em `uninstall.exe` ao lado da app — e quando o
//! nome do próprio ficheiro é esse, arranca em modo de desinstalação. Um binário, dois
//! papéis, zero ficheiros extra.
//!
//! # Elevação
//!
//! O manifesto é `asInvoker` e a elevação é pedida em runtime: se não formos
//! administradores, relança-se o próprio exe com o verbo `runas` e sai-se. É o que torna
//! o instalador testável — `--teste --dir=<pasta>` corre a instalação inteira numa pasta
//! qualquer, sem UAC, sem registo e sem atalhos, e é assim que a máquina que o compila o
//! consegue verificar sem ninguém a clicar em janelas.

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use tauri::Emitter;

/// A app embutida, comprimida no build.
static PAYLOAD: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/bruma.exe.zst"));
const VERSAO: &str = env!("VERSAO_DA_APP");

const CHAVE_DESINSTALACAO: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\Bruma";

#[derive(Clone, Copy, PartialEq)]
enum Modo {
    Instalar,
    Desinstalar,
}

struct Opcoes {
    modo: Modo,
    silencioso: bool,
    teste: bool,
    dir: Option<PathBuf>,
    /// `/R` do updater: relançar a app no fim.
    relancar: bool,
    /// `/UPDATE`: é uma atualização, não uma primeira instalação — os atalhos que a
    /// pessoa tenha apagado não voltam a aparecer-lhe na área de trabalho.
    atualizacao: bool,
    /// O que vier depois de `/ARGS`: os argumentos com que a app deve renascer.
    args_da_app: Vec<String>,
}

fn opcoes() -> Opcoes {
    let exe = std::env::current_exe().unwrap_or_default();
    let sou_uninstall = exe
        .file_name()
        .map(|n| n.to_string_lossy().eq_ignore_ascii_case("uninstall.exe"))
        .unwrap_or(false);

    let mut o = Opcoes {
        modo: if sou_uninstall {
            Modo::Desinstalar
        } else {
            Modo::Instalar
        },
        silencioso: false,
        teste: false,
        dir: None,
        relancar: false,
        atualizacao: false,
        args_da_app: Vec::new(),
    };
    let mut resto = std::env::args().skip(1);
    while let Some(a) = resto.next() {
        match a.as_str() {
            "--uninstall" => o.modo = Modo::Desinstalar,
            // O dialeto do updater do Tauri: /P (passivo) e /S (silencioso) são, para
            // nós, a mesma coisa — instala sem interface. /R relança a app no fim.
            "/S" | "/P" | "--silencioso" => o.silencioso = true,
            "/R" => o.relancar = true,
            "/UPDATE" => o.atualizacao = true,
            // Tudo depois de /ARGS pertence à app, não a nós. É o último a aparecer.
            "/ARGS" => {
                o.args_da_app = resto.collect();
                break;
            }
            "--teste" => o.teste = true,
            _ => {
                if let Some(d) = a.strip_prefix("--dir=") {
                    o.dir = Some(PathBuf::from(d));
                } else if let Some(d) = a.strip_prefix("_?=") {
                    // Compatibilidade com quem fala NSIS: instalações antigas invocam o
                    // desinstalador com esta forma.
                    o.dir = Some(PathBuf::from(d));
                }
            }
        }
    }
    o
}

/// Onde agir quando ninguém disse: instalar vai para o sítio do costume; desinstalar
/// age na pasta onde ESTE executável vive.
///
/// A segunda parte não é estética. O desinstalador fica dentro da pasta da instalação
/// por construção — é essa que ele deve limpar. Ir ao registo perguntar "onde está o
/// Bruma?" parecia mais correto e era mais perigoso: uma cópia do uninstall.exe corrida
/// de outro sítio iria apagar uma instalação que não é a dela. Foi o próprio teste que
/// tropeçou nisto — apontou ao Program Files verdadeiro e só as permissões o travaram.
fn destino_por_omissao(modo: Modo) -> PathBuf {
    if modo == Modo::Desinstalar {
        if let Ok(eu) = std::env::current_exe() {
            if let Some(pai) = eu.parent() {
                return pai.to_path_buf();
            }
        }
    }
    dir_instalada().unwrap_or_else(dir_padrao)
}

fn dir_padrao() -> PathBuf {
    std::env::var("ProgramFiles")
        .map(|p| PathBuf::from(p).join("Bruma"))
        .unwrap_or_else(|_| PathBuf::from(r"C:\Program Files\Bruma"))
}

fn dir_de_dados() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("Bruma"))
}

/* ========================================================================== elevação */

#[cfg(windows)]
fn sou_administrador() -> bool {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut info = TOKEN_ELEVATION::default();
        let mut n = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut info as *mut _ as *mut _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut n,
        );
        let _ = windows::Win32::Foundation::CloseHandle(token);
        ok.is_ok() && info.TokenIsElevated != 0
    }
}

/// Relança este exe como administrador e, se `esperar`, aguarda que ele termine.
///
/// A espera importa no caminho do auto-update: quem relança a app no fim tem de ser o
/// processo NÃO elevado — uma app de conversas não tem nada que herdar privilégios de
/// administrador. O pai fica à espera do filho elevado acabar de instalar, e é ele que
/// abre o Bruma novo, já sem poderes nenhuns.
#[cfg(windows)]
fn relancar_como_administrador(esperar: bool) -> Result<i32> {
    use windows::core::{w, PCWSTR};
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE};
    use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let exe = std::env::current_exe()?;
    let exe_w: Vec<u16> = exe.to_string_lossy().encode_utf16().chain([0]).collect();
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Argumentos com espaços voltam entre aspas, senão chegam partidos ao filho.
    let juntos = args
        .iter()
        .map(|a| {
            if a.contains(' ') {
                format!("\"{a}\"")
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    let args_w: Vec<u16> = juntos.encode_utf16().chain([0]).collect();

    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(exe_w.as_ptr()),
        lpParameters: PCWSTR(args_w.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    unsafe {
        ShellExecuteExW(&mut info).map_err(|_| anyhow!("a elevação foi recusada"))?;
        if !esperar || info.hProcess.is_invalid() {
            return Ok(0);
        }
        WaitForSingleObject(info.hProcess, INFINITE);
        let mut codigo = 0u32;
        let _ = GetExitCodeProcess(info.hProcess, &mut codigo);
        let _ = windows::Win32::Foundation::CloseHandle(info.hProcess);
        Ok(codigo as i32)
    }
}

/* ========================================================================== passos */

fn avisar(janela: Option<&tauri::WebviewWindow>, passo: &str) {
    if let Some(j) = janela {
        let _ = j.emit("passo", passo);
    }
    println!("[instalador] {passo}");
    anotar(passo);
}

/// Escreve no mesmo registo da app.
///
/// # Porque é que isto tem de existir
///
/// A app não sabe se a actualização correu bem. O plugin do Tauri lança o instalador e
/// chama `exit(0)` a seguir: não espera, não lê o código de saída. Se o UAC for recusado,
/// se a extracção falhar, se o registo não deixar escrever — a app **fechou-se e não
/// volta**, e não havia uma linha em lado nenhum.
///
/// A falha típica era "cliquei em Atualizar e o Bruma desapareceu". Não se consegue evitar
/// o desaparecimento — mas consegue-se deixar dito até onde é que ele chegou.
fn anotar(linha: &str) {
    use std::io::Write;
    let Some(base) = std::env::var_os("APPDATA") else {
        return;
    };
    let destino = std::path::PathBuf::from(base)
        .join("Bruma")
        .join("bruma.log");
    if let Some(pai) = destino.parent() {
        let _ = std::fs::create_dir_all(pai);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&destino)
    {
        let _ = writeln!(f, "[instalador] {linha}");
    }
}

fn fechar_o_bruma() {
    use std::os::windows::process::CommandExt;
    // 0x08000000 = CREATE_NO_WINDOW: sem consolas a piscar no meio da instalação.
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/IM", "bruma.exe"])
        .creation_flags(0x0800_0000)
        .status();
}

/// Remove a instalação antiga por utilizador, se existir.
///
/// CAUTELA, porque isto corre elevado: nunca se executa o que o registo diga. Um
/// `UninstallString` em HKCU é escrevível sem privilégios — executá-lo às cegas num
/// processo administrador seria oferecer elevação a qualquer coisa. Só se aceita o
/// caminho exato onde as versões antigas sempre se instalaram.
fn remover_instalacao_por_utilizador(janela: Option<&tauri::WebviewWindow>) {
    let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) else {
        return;
    };
    let antiga = local.join("Bruma");
    let desinstalador = antiga.join("uninstall.exe");
    if desinstalador.exists() {
        avisar(janela, "a remover a instalação antiga (por utilizador)");
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new(&desinstalador)
            .args(["/S", &format!("_?={}", antiga.display())])
            .creation_flags(0x0800_0000)
            .status();
        let _ = std::fs::remove_file(&desinstalador);
        let _ = std::fs::remove_dir_all(&antiga);
    }
    let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    let _ = hkcu.delete_subkey_all(CHAVE_DESINSTALACAO);
}

fn extrair_a_app(destino: &Path, janela: Option<&tauri::WebviewWindow>) -> Result<()> {
    if PAYLOAD.is_empty() {
        return Err(anyhow!(
            "este instalador foi compilado sem a aplicação lá dentro (build de verificação)"
        ));
    }
    avisar(janela, "a instalar a aplicação");
    std::fs::create_dir_all(destino)
        .with_context(|| format!("não consegui criar {}", destino.display()))?;
    let bytes = zstd::decode_all(PAYLOAD).context("o conteúdo embutido está corrompido")?;
    // Escreve-se ao lado e renomeia-se no fim: ou fica a versão nova inteira, ou fica a
    // antiga intacta. Nunca meia app.
    let temporario = destino.join("bruma.exe.novo");
    std::fs::write(&temporario, &bytes).context("não consegui escrever a aplicação")?;
    std::fs::rename(&temporario, destino.join("bruma.exe"))
        .context("não consegui substituir a aplicação")?;
    Ok(())
}

fn copiar_me_como_desinstalador(destino: &Path) -> Result<()> {
    let eu = std::env::current_exe()?;
    let alvo = destino.join("uninstall.exe");
    // Se já estivermos a correr DE lá (reinstalação por cima), não há nada a copiar.
    if eu.canonicalize().ok() == alvo.canonicalize().ok() {
        return Ok(());
    }
    std::fs::copy(&eu, &alvo).context("não consegui criar o desinstalador")?;
    Ok(())
}

fn escrever_registo(destino: &Path) -> Result<()> {
    use winreg::enums::*;
    let hklm = winreg::RegKey::predef(HKEY_LOCAL_MACHINE);
    let (chave, _) = hklm
        .create_subkey(CHAVE_DESINSTALACAO)
        .context("não consegui escrever no registo — isto precisa de administrador")?;
    let dir = destino.display().to_string();
    chave.set_value("DisplayName", &"Bruma")?;
    chave.set_value("DisplayVersion", &VERSAO)?;
    chave.set_value("Publisher", &"Bruma")?;
    chave.set_value("DisplayIcon", &format!("{dir}\\bruma.exe"))?;
    chave.set_value("InstallLocation", &format!("\"{dir}\""))?;
    chave.set_value("UninstallString", &format!("\"{dir}\\uninstall.exe\""))?;
    // O canal de atualização (NSIS, silencioso) lê este nome para saber o que reiniciar.
    chave.set_value("MainBinaryName", &"bruma.exe")?;
    chave.set_value("NoModify", &1u32)?;
    chave.set_value("NoRepair", &1u32)?;
    let kb = (PAYLOAD.len() as u32) / 1024;
    chave.set_value("EstimatedSize", &kb)?;
    Ok(())
}

#[cfg(windows)]
fn criar_atalho(lnk: &Path, alvo: &Path) -> Result<()> {
    use windows::core::{Interface, HSTRING, PCWSTR};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, IPersistFile, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    unsafe {
        // Pode já estar inicializado por outra coisa; RPC_E_CHANGED_MODE não é fatal.
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        let alvo_h = HSTRING::from(alvo.as_os_str());
        link.SetPath(PCWSTR(alvo_h.as_ptr()))?;
        let pasta = alvo.parent().unwrap_or(Path::new(""));
        let pasta_h = HSTRING::from(pasta.as_os_str());
        link.SetWorkingDirectory(PCWSTR(pasta_h.as_ptr()))?;
        let ficheiro: IPersistFile = link.cast()?;
        let lnk_h = HSTRING::from(lnk.as_os_str());
        ficheiro.Save(PCWSTR(lnk_h.as_ptr()), true)?;
    }
    Ok(())
}

fn atalhos(
    destino: &Path,
    area_de_trabalho: bool,
    teste: bool,
    janela: Option<&tauri::WebviewWindow>,
) -> Result<()> {
    avisar(janela, "a criar os atalhos");
    let exe = destino.join("bruma.exe");
    if teste {
        // Em teste os atalhos vão para dentro da própria pasta: prova-se que a criação
        // funciona sem tocar no menu Iniciar de ninguém.
        criar_atalho(&destino.join("Bruma.lnk"), &exe)?;
        return Ok(());
    }
    let programas = std::env::var("ProgramData")
        .map(|p| PathBuf::from(p).join(r"Microsoft\Windows\Start Menu\Programs"))
        .context("sem ProgramData")?;
    criar_atalho(&programas.join("Bruma.lnk"), &exe)?;
    if area_de_trabalho {
        if let Ok(publico) = std::env::var("PUBLIC") {
            criar_atalho(
                &PathBuf::from(publico).join("Desktop").join("Bruma.lnk"),
                &exe,
            )?;
        }
    }
    Ok(())
}

fn instalar(
    destino: &Path,
    area_de_trabalho: bool,
    atualizacao: bool,
    teste: bool,
    janela: Option<&tauri::WebviewWindow>,
) -> Result<()> {
    avisar(janela, "a fechar o Bruma, se estiver aberto");
    if !teste {
        fechar_o_bruma();
        remover_instalacao_por_utilizador(janela);
    }
    extrair_a_app(destino, janela)?;
    copiar_me_como_desinstalador(destino)?;
    // Numa atualização não se recria o atalho da área de trabalho: se a pessoa o apagou,
    // apagado fica — reaparecer a cada versão é dos hábitos mais irritantes que há.
    atalhos(destino, area_de_trabalho && !atualizacao, teste, janela)?;
    if !teste {
        avisar(janela, "a registar a instalação");
        escrever_registo(destino)?;
    }
    avisar(janela, "pronto");
    Ok(())
}

fn desinstalar(destino: &Path, apagar_dados: bool, teste: bool) -> Result<()> {
    if !teste {
        fechar_o_bruma();
    }
    // Atalhos e registo primeiro; os ficheiros por último, porque o nosso próprio exe
    // está entre eles e só se apaga a si mesmo no fim, por fora.
    if !teste {
        if let Ok(pd) = std::env::var("ProgramData") {
            let _ = std::fs::remove_file(
                PathBuf::from(pd).join(r"Microsoft\Windows\Start Menu\Programs\Bruma.lnk"),
            );
        }
        if let Ok(publico) = std::env::var("PUBLIC") {
            let _ = std::fs::remove_file(PathBuf::from(publico).join(r"Desktop\Bruma.lnk"));
        }
        let hklm = winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE);
        let _ = hklm.delete_subkey_all(CHAVE_DESINSTALACAO);
    }

    if apagar_dados {
        // A pessoa marcou a caixa que diz PARA SEMPRE. Cumpre-se.
        if let Some(dados) = dir_de_dados() {
            let _ = std::fs::remove_dir_all(dados);
        }
    }

    let _ = std::fs::remove_file(destino.join("bruma.exe"));

    // O exe não se apaga a si próprio enquanto corre. Deixa-se um script a fazê-lo
    // depois de sairmos — e num FICHEIRO, não numa linha de comandos: um `for /l` passado
    // por `cmd /C` é reinterpretado e corre uma volta só, o que deixava o uninstall.exe
    // para trás. Num .cmd o parsing é o normal e o ciclo cumpre-se.
    //
    // O caminho vai sempre com barras invertidas: o `del` e o `rmdir` tratam a barra
    // normal como início de switch e recusam o caminho inteiro.
    use std::os::windows::process::CommandExt;
    let dir = destino.display().to_string().replace('/', "\\");
    let script = std::env::temp_dir().join("bruma-limpar.cmd");
    // Linha a linha, para o .cmd sair sem indentacao: um `:etiqueta` do cmd tem de
    // comecar na coluna zero, senao o `goto` nao a encontra.
    // Apagam-se os ficheiros PELO NOME, um a um, e nao com um asterisco.
    //
    // A pasta de instalacao escreve-se a mao num campo de texto da interface. Quem
    // escrevesse la a pasta do Ambiente de Trabalho ficava com um desinstalador que,
    // mais tarde, apagava tudo o que la estivesse. O instalador poe QUATRO ficheiros
    // nesta pasta, e sao esses quatro que ele tem o direito de tirar.
    //
    // O `rmdir` sem `/S` e de proposito: so remove a pasta se ela ficar vazia. Se la
    // estiver mais alguma coisa -- a pasta `dados`, ou o que a pessoa la tinha -- a
    // pasta fica, e e isso que tem de acontecer.
    let nossos = ["bruma.exe", "bruma.exe.novo", "uninstall.exe", "Bruma.lnk"];
    let mut linhas = vec![
        "@echo off".to_string(),
        "set n=0".to_string(),
        ":tentar".to_string(),
        "set /a n+=1".to_string(),
        "ping -n 2 127.0.0.1 >nul".to_string(),
    ];
    for f in nossos {
        linhas.push(format!("del /F /Q \"{dir}\\{f}\" >nul 2>&1"));
    }
    linhas.extend([
        // O nosso proprio exe e o ultimo a ceder: enquanto ele la estiver, volta-se.
        format!("if exist \"{dir}\\uninstall.exe\" if %n% lss 15 goto tentar"),
        format!("rmdir \"{dir}\" >nul 2>&1"),
        // `del "%~f0"` a meio do ficheiro faz o cmd perder-se e escrever "nao e
        // possivel encontrar o arquivo em lotes". O `(goto) 2>nul` fecha-o primeiro.
        "(goto) 2>nul & del \"%~f0\"".to_string(),
    ]);
    let conteudo = linhas.join("\r\n") + "\r\n";
    if std::fs::write(&script, conteudo).is_ok() {
        let _ = std::process::Command::new("cmd")
            .args(["/C", &script.display().to_string()])
            .creation_flags(0x0800_0000)
            .spawn();
    }
    Ok(())
}

/// Abre o Bruma tal como estamos: usado pelo processo NÃO elevado depois da instalação.
fn abrir_a_app(destino: &Path, args: &[String]) {
    let _ = std::process::Command::new(destino.join("bruma.exe"))
        .args(args)
        // cwd fixo na pasta da instalação: herdar o de quem invocou o updater faria a
        // app procurar a pasta `dados` em sítios diferentes conforme o dia.
        .current_dir(destino)
        .spawn();
}

/// Abre o Bruma a partir de um processo elevado, largando os privilégios pelo caminho.
/// O explorer não passa argumentos — é o preço deste atalho, e a app não usa nenhuns.
fn abrir_a_app_sem_privilegios(destino: &Path) {
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new("explorer.exe")
        .arg(destino.join("bruma.exe"))
        .creation_flags(0x0800_0000)
        .spawn();
}

/* ========================================================================== comandos */

struct Estado {
    opcoes: Opcoes,
}

#[derive(serde::Serialize)]
struct Info {
    versao: String,
    modo: String,
    dir: String,
    dados: Option<String>,
    ja_instalado: bool,
}

#[tauri::command]
fn info(estado: tauri::State<Estado>) -> Info {
    let dir = estado
        .opcoes
        .dir
        .clone()
        .unwrap_or_else(|| destino_por_omissao(estado.opcoes.modo));
    Info {
        versao: VERSAO.into(),
        modo: match estado.opcoes.modo {
            Modo::Instalar => "instalar".into(),
            Modo::Desinstalar => "desinstalar".into(),
        },
        dir: dir.display().to_string(),
        dados: dir_de_dados().map(|d| d.display().to_string()),
        ja_instalado: dir_instalada().is_some(),
    }
}

/// Onde o Bruma já está instalado, segundo o registo — sem executar nada de lá.
fn dir_instalada() -> Option<PathBuf> {
    let hklm = winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE);
    let chave = hklm.open_subkey(CHAVE_DESINSTALACAO).ok()?;
    let bruto: String = chave.get_value("InstallLocation").ok()?;
    let limpo = bruto.trim_matches('"');
    if limpo.is_empty() {
        None
    } else {
        Some(PathBuf::from(limpo))
    }
}

#[tauri::command]
fn correr_instalacao(
    janela: tauri::WebviewWindow,
    estado: tauri::State<Estado>,
    dir: String,
    atalho: bool,
) -> Result<(), String> {
    instalar(
        &PathBuf::from(dir),
        atalho,
        estado.opcoes.atualizacao,
        estado.opcoes.teste,
        Some(&janela),
    )
    .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn correr_desinstalacao(
    estado: tauri::State<Estado>,
    dir: String,
    apagar_dados: bool,
) -> Result<(), String> {
    desinstalar(&PathBuf::from(dir), apagar_dados, estado.opcoes.teste)
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn abrir_e_sair(app: tauri::AppHandle, dir: String) {
    use std::os::windows::process::CommandExt;
    // Pelo explorer, para a app abrir SEM os privilégios do instalador. Uma app de
    // conversas não tem nada que correr como administrador.
    let _ = std::process::Command::new("explorer.exe")
        .arg(format!("{dir}\\bruma.exe"))
        .creation_flags(0x0800_0000)
        .spawn();
    app.exit(0);
}

/// A interface descreve-se a si própria, para se poder verificar sem olhos.
///
/// Fotografar a janela não chega: o `PrintWindow` devolve a WebView2 incompleta e
/// trazê-la à frente a partir de outro processo é bloqueado pelo Windows.
#[tauri::command]
fn medir(linha: String) {
    println!("[ui] {linha}");
}

#[tauri::command]
fn sair(app: tauri::AppHandle) {
    app.exit(0);
}

/* ========================================================================== arranque */

fn main() {
    let o = opcoes();

    // Sem interface: instala ou desinstala e sai. É também o caminho da verificação
    // automática (--teste --dir=…) e o que um canal NSIS antigo invoca com /S.
    if o.silencioso {
        let destino = o.dir.clone().unwrap_or_else(|| destino_por_omissao(o.modo));
        if !o.teste && !sou_administrador() {
            // O pai (não elevado) espera pelo filho elevado e é ELE que relança a app —
            // de propósito, para o Bruma novo nascer sem privilégios de administrador.
            match relancar_como_administrador(true) {
                Ok(0) => {
                    if o.relancar {
                        abrir_a_app(&destino, &o.args_da_app);
                    }
                    return;
                }
                Ok(codigo) => std::process::exit(codigo),
                Err(_) => std::process::exit(1),
            }
        }
        let r = match o.modo {
            Modo::Instalar => instalar(&destino, true, o.atualizacao, o.teste, None),
            Modo::Desinstalar => desinstalar(&destino, false, o.teste),
        };
        if let Err(e) = r {
            eprintln!("[instalador] falhou: {e:#}");
            anotar(&format!("FALHOU: {e:#}"));
            std::process::exit(1);
        }
        // Já elevados e sem pai à espera (--teste, ou alguém correu-nos já como admin):
        // relança-se daqui na mesma, que é melhor do que não relançar de todo.
        if o.relancar && o.teste {
            abrir_a_app(&destino, &o.args_da_app);
        } else if o.relancar && sou_administrador() && o.dir.is_none() {
            abrir_a_app_sem_privilegios(&destino);
        }
        return;
    }

    // Com interface: eleva primeiro, para os botões poderem cumprir o que prometem.
    if !o.teste && !sou_administrador() {
        let _ = relancar_como_administrador(false);
        return; // elevado ou recusado, este processo já não tem papel
    }

    tauri::Builder::default()
        .manage(Estado { opcoes: o })
        .invoke_handler(tauri::generate_handler![
            info,
            correr_instalacao,
            correr_desinstalacao,
            medir,
            abrir_e_sair,
            sair
        ])
        .run(tauri::generate_context!())
        .expect("o instalador não conseguiu abrir a janela");
}
