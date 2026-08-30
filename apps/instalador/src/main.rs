// SEM A CONSOLA PRETA (#180).
//
// A app tem este mesmo atributo desde sempre; o instalador não tinha nada. Um binário Rust sem
// ele é do subsistema de CONSOLA: ao fazer duplo clique no `Instalar-Bruma.exe`, o Windows
// aloca uma janela preta por trás da nossa — e a elevação por `ShellExecuteExW` abre outra. A
// primeira coisa que a pessoa vê ao instalar uma app de conversas é um terminal.
//
// Só em release. Em debug a consola fica, porque é onde os `println!` do `medir` aparecem
// quando se está a trabalhar.
//
// E para o CI não perder a saída: o `agarrar_a_consola_do_pai` liga-se à consola de quem nos
// chamou, quando existe uma. Corrido do PowerShell, os `println!` continuam a sair; corrido do
// Explorador, não há consola nenhuma a que se ligar e não se abre nada.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

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

use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};
use tauri::Emitter;

/// A app embutida, comprimida no build.
static PAYLOAD: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/bruma.exe.zst"));
const VERSAO: &str = env!("VERSAO_DA_APP");

const CHAVE_DESINSTALACAO: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\Bruma";

/// O que uma instalação ou desinstalação tem a dizer para lá de «correu bem».
///
/// Era `Result<()>`: ou tudo, ou nada. E havia coisas no meio — um atalho que não se criou
/// depois de a app já estar instalada, uma identidade que não se encontrou onde se prometeu
/// apagá-la. Essas não são erros (o essencial aconteceu) nem são silêncio (a pessoa tem de
/// saber). Passam a caber aqui, e a interface mostra-as.
#[derive(serde::Serialize, Default)]
struct Resultado {
    /// Correu mal, mas não impediu o essencial. Vai para o ecrã e para o registo.
    avisos: Vec<String>,
    /// Quantas pastas de dados foram MESMO apagadas. Zero com o `apagar_dados` ligado quer
    /// dizer que não se encontrou nenhuma — e nesse caso não se pode dizer que se apagou.
    dados_apagados: usize,
}

#[derive(Clone, Copy, PartialEq)]
enum Modo {
    Instalar,
    Desinstalar,
}

struct Opcoes {
    modo: Modo,
    silencioso: bool,
    teste: bool,
    apagar_dados: bool,
    dir: Option<PathBuf>,
    /// `/R` do updater: relançar a app no fim.
    relancar: bool,
    /// `/UPDATE`: é uma atualização, não uma primeira instalação — os atalhos que a
    /// pessoa tenha apagado não voltam a aparecer-lhe na área de trabalho.
    atualizacao: bool,
    /// O que vier depois de `/ARGS`: os argumentos com que a app deve renascer.
    args_da_app: Vec<String>,
}

/// Lê o `argv` real. Três linhas, porque a decisão está toda no [`opcoes_de`].
fn opcoes() -> Opcoes {
    let exe = std::env::current_exe().unwrap_or_default();
    let nome = exe.file_name().map(|n| n.to_string_lossy().into_owned());
    opcoes_de(nome.as_deref().unwrap_or(""), std::env::args().skip(1))
}

/// O que os argumentos QUEREM DIZER — sem tocar no ambiente.
///
/// Isto lia o `std::env::args()` e o `current_exe()` lá dentro, o que o tornava impossível de
/// testar de fora: por isso não tinha um único teste, apesar de decidir tudo o que o instalador
/// faz a seguir — se instala ou desinstala, se pede elevação, se apaga a identidade, e o que
/// passa à app quando a relança.
///
/// O dialecto do updater (`/P /R /UPDATE /ARGS`) é o caminho por onde passam **todas** as
/// instalações existentes, e era o menos exercitado de todos.
fn opcoes_de(exe_nome: &str, args: impl Iterator<Item = String>) -> Opcoes {
    let sou_uninstall = exe_nome.eq_ignore_ascii_case("uninstall.exe");

    let mut o = Opcoes {
        modo: if sou_uninstall {
            Modo::Desinstalar
        } else {
            Modo::Instalar
        },
        silencioso: false,
        teste: false,
        apagar_dados: false,
        dir: None,
        relancar: false,
        atualizacao: false,
        args_da_app: Vec::new(),
    };
    let mut resto = args;
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
            // O caminho silencioso passava sempre `false`, e por isso o ramo que apaga a
            // identidade nunca corria fora da interface -- justamente o ramo que dizia
            // "apaguei" sem olhar.
            "--apagar-dados" => o.apagar_dados = true,
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

/// Onde a identidade pode estar, por ordem de probabilidade.
///
/// A app nao guarda os dados num sitio so: `estado.rs` prefere uma pasta `dados` ao lado
/// -- e a app instalada corre com o cwd fixo na pasta da instalacao. Instalacoes antigas
/// vivem assim. O desinstalador so olhava para o `%APPDATA%`, portanto quem tivesse a
/// identidade ao lado do executavel marcava "apagar para sempre", ouvia que tinha sido
/// apagada, e ficava com ela intacta no disco.
fn sitios_dos_dados(destino: &Path, teste: bool) -> Vec<PathBuf> {
    let mut v = Vec::new();
    // Em teste NAO se toca no %APPDATA%: e a pasta de dados a serio de quem esta a correr
    // o teste. Um portao de verificacao que apaga a identidade do dono e pior do que nao
    // existir portao nenhum.
    if !teste {
        if let Some(d) = dir_de_dados() {
            v.push(d);
        }
    }
    v.push(destino.join("dados"));
    // E onde a pessoa a tiver posto à mão. Quem usa o `BRUMA_DADOS` é precisamente quem
    // mudou a pasta de sítio — dizer-lhe que se apagou a identidade sem sequer olhar para lá
    // seria a mesma mentira, com um passo a mais.
    if let Some(d) = std::env::var_os("BRUMA_DADOS") {
        let d = PathBuf::from(d);
        if !v.contains(&d) {
            v.push(d);
        }
    }
    v
}

/// Liga-se à consola de quem nos chamou, se houver uma.
///
/// Existe por causa do `windows_subsystem = "windows"` lá em cima: sem ele o instalador abria
/// uma consola preta ao duplo clique; com ele, e sem isto, o CI perdia todos os `println!` — e
/// o portão da release lê-os para saber o que aconteceu.
///
/// `ATTACH_PARENT_PROCESS` falha quando não há consola de pai (duplo clique no Explorador), e
/// falhar é exactamente o comportamento certo: não se cria nenhuma.
#[cfg(windows)]
fn agarrar_a_consola_do_pai() {
    use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(not(windows))]
fn agarrar_a_consola_do_pai() {}

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
    //
    // Os DOIS têm de responder. Isto era `eu.canonicalize().ok() == alvo.canonicalize().ok()`,
    // e quando ambos falhavam os dois lados davam `None` — que são iguais. A função saía com
    // `Ok` sem ter copiado nada, e a instalação ficava sem desinstalador: nada no registo o
    // apagaria, e a pessoa só descobria no dia em que quisesse remover a app.
    //
    // `canonicalize` falha quando o ficheiro não existe, e o `uninstall.exe` não existe
    // justamente na primeira instalação — o caso mais comum de todos.
    if let (Ok(a), Ok(b)) = (eu.canonicalize(), alvo.canonicalize()) {
        if a == b {
            return Ok(());
        }
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
) -> Result<Resultado> {
    let mut r = Resultado::default();
    avisar(janela, "a fechar o Bruma, se estiver aberto");
    if !teste {
        fechar_o_bruma();
        remover_instalacao_por_utilizador(janela);
    }
    extrair_a_app(destino, janela)?;
    copiar_me_como_desinstalador(destino)?;

    // O DESINSTALADOR TEM DE ESTAR LÁ, E AFIRMA-SE.
    //
    // O `copiar_me_como_desinstalador` tem um ramo que devolve `Ok` sem copiar. Se alguma vez
    // voltar a poder sair por engano, é aqui que se sabe — e não meses depois, quando alguém
    // quiser remover a app e não encontrar nada.
    let desinstalador = destino.join("uninstall.exe");
    if !desinstalador.exists() {
        bail!(
            "instalei a app mas não ficou desinstalador em {}",
            desinstalador.display()
        );
    }

    // UM ATALHO QUE FALHA NÃO DESFAZ UMA INSTALAÇÃO QUE JÁ ACONTECEU (#182).
    //
    // Isto era `atalhos(...)?`. Qualquer falha do COM ou do `IPersistFile` abortava a
    // instalação DEPOIS de o `bruma.exe` já ter sido substituído: a pessoa via o ecrã de erro,
    // concluía que nada tinha sido instalado, e a app nova estava lá. Numa ACTUALIZAÇÃO isso é
    // pior ainda — a versão nova já ficou, e a mensagem diz que falhou.
    //
    // Extrair e registar continuam fatais; o atalho é conveniência. Passa a queixar-se e a
    // seguir — para o registo E para o ecrã, porque um aviso só no registo é um aviso que
    // ninguém lê.
    if let Err(e) = atalhos(destino, area_de_trabalho && !atualizacao, teste, janela) {
        let aviso = format!("instalei, mas não consegui criar o atalho: {e:#}");
        anotar(&aviso);
        r.avisos.push(aviso);
    }
    if !teste {
        avisar(janela, "a registar a instalação");
        escrever_registo(destino)?;
    }
    avisar(janela, "pronto");
    Ok(r)
}

fn desinstalar(destino: &Path, apagar_dados: bool, teste: bool) -> Result<Resultado> {
    let mut resultado = Resultado::default();
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

    let mut falha_dos_dados: Option<Vec<String>> = None;
    if apagar_dados {
        // A pessoa marcou a caixa que diz PARA SEMPRE. Cumpre-se -- e CONFIRMA-SE.
        //
        // Isto era um `let _ =` sobre um `remove_dir_all`, e a interface dizia "a
        // identidade foi apagada, como pediste" a seguir, sem olhar. Se a remocao
        // falhasse -- pasta em uso, permissoes, ou simplesmente noutro sitio -- a pessoa
        // ficava convencida de que a identidade tinha desaparecido do mundo enquanto ela
        // continuava no disco. E a mentira mais grave que esta app podia contar.
        let mut queixas = Vec::new();
        let procurados = sitios_dos_dados(destino, teste);
        for dados in &procurados {
            if !dados.exists() {
                continue;
            }
            if let Err(e) = std::fs::remove_dir_all(dados) {
                queixas.push(format!("{}: {e}", dados.display()));
            } else if dados.exists() {
                queixas.push(format!("{}: ficou la depois de apagar", dados.display()));
            } else {
                resultado.dados_apagados += 1;
            }
        }

        // E O CASO QUE FALTAVA: NAO HAVIA NADA PARA APAGAR (#183).
        //
        // A metade dificil ja estava feita -- uma remocao que FALHA deixou de ser silencio.
        // Mas quando nao se encontrava nada, as queixas ficavam vazias, a funcao devolvia Ok,
        // e a interface dizia na mesma «a identidade foi apagada, como pediste». Zero pastas
        // encontradas e zero pastas apagadas produziam a mesma frase.
        //
        // Nao e erro: nao ha nada de errado em desinstalar uma app cujos dados estao noutro
        // sitio. E um FACTO, e a pessoa esta a decidir uma coisa irreversivel -- tem de saber
        // que ela nao aconteceu, e onde e que se procurou.
        if resultado.dados_apagados == 0 && queixas.is_empty() {
            let onde: Vec<String> = procurados.iter().map(|p| p.display().to_string()).collect();
            resultado.avisos.push(format!(
                "nao encontrei identidade nenhuma nos sitios onde ela costuma estar ({}); \
                 se puseste os dados noutro lado, ainda la estao",
                onde.join(", ")
            ));
        }
        // A queixa fica para o FIM: a mensagem promete que o resto foi desinstalado, e
        // isso so e verdade depois de o resto ter sido mesmo desinstalado.
        falha_dos_dados = Some(queixas);
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

    if let Some(queixas) = falha_dos_dados.filter(|q| !q.is_empty()) {
        bail!(
            "o Bruma foi desinstalado, mas NAO consegui apagar a identidade: {}. Ela continua no disco -- apaga essa pasta a mao para a perderes mesmo.",
            queixas.join("; ")
        );
    }
    for a in &resultado.avisos {
        anotar(a);
    }
    Ok(resultado)
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
) -> Result<Resultado, String> {
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
) -> Result<Resultado, String> {
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
    agarrar_a_consola_do_pai();
    let o = opcoes();

    // Sem interface: instala ou desinstala e sai. É também o caminho da verificação
    // automática (--teste --dir=…) e o que um canal NSIS antigo invoca com /S.
    if o.silencioso {
        // O QUE NOS PEDIRAM, ANTES DE FAZER SEJA O QUE FOR (#59).
        //
        // O caminho silencioso é o do auto-update: ninguém está a olhar. Sem esta linha não há
        // como distinguir «o updater nunca me chamou» de «chamou e eu recusei» — as duas
        // acabam com a app na versão antiga e o registo calado.
        anotar(&format!(
            "arranquei em silencio com: {}",
            std::env::args().skip(1).collect::<Vec<_>>().join(" ")
        ));
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
                // O DESAPARECIMENTO PASSA A DEIXAR DITO ATE ONDE CHEGOU (#59).
                //
                // Estes dois `exit` saíam sem uma linha. O comentário do `anotar` promete
                // exactamente o contrário — «não se consegue evitar o desaparecimento, mas
                // consegue-se deixar dito até onde é que ele chegou» — e o caminho onde isso
                // mais importa era o único que não o cumpria.
                //
                // O UAC recusado é o caso comum: a pessoa carrega em «Não», a janela some, a
                // app reabre na versão antiga, e não há nada em lado nenhum a explicar porquê.
                Ok(codigo) => {
                    anotar(&format!(
                        "o instalador elevado saiu com o codigo {codigo} -- a actualizacao NAO \
                         foi instalada, a app continua na versao anterior"
                    ));
                    std::process::exit(codigo)
                }
                Err(e) => {
                    anotar(&format!(
                        "a elevacao foi recusada ou falhou ({e:#}) -- a actualizacao NAO foi \
                         instalada, a app continua na versao anterior"
                    ));
                    std::process::exit(1)
                }
            }
        }
        let r = match o.modo {
            Modo::Instalar => instalar(&destino, true, o.atualizacao, o.teste, None),
            Modo::Desinstalar => desinstalar(&destino, o.apagar_dados, o.teste),
        };
        match r {
            Err(e) => {
                eprintln!("[instalador] falhou: {e:#}");
                anotar(&format!("FALHOU: {e:#}"));
                std::process::exit(1);
            }
            Ok(resultado) => {
                for a in &resultado.avisos {
                    eprintln!("[instalador] aviso: {a}");
                }
            }
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
        if let Err(e) = relancar_como_administrador(false) {
            anotar(&format!(
                "a elevacao para a janela falhou ou foi recusada ({e:#})"
            ));
        }
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

#[cfg(test)]
mod testes {
    use super::*;

    fn de(exe: &str, args: &[&str]) -> Opcoes {
        opcoes_de(exe, args.iter().map(|a| a.to_string()))
    }

    /// O DIALECTO DO UPDATER, QUE É POR ONDE PASSAM TODAS AS ACTUALIZAÇÕES.
    ///
    /// O portão da release corre `--silencioso --teste --dir=…`. Mas o `tauri.conf.json`
    /// declara `installMode: passive`, e o updater corre o nosso exe com `/P /R /UPDATE /ARGS`.
    /// Esses quatro estavam implementados e não eram exercitados por nada — nem por um teste,
    /// nem pelo portão. O caminho mais usado do programa era o menos verificado.
    #[test]
    fn o_dialecto_do_updater_le_se_todo() {
        let o = de(
            "Instalar-Bruma.exe",
            &["/P", "/R", "/UPDATE", "/ARGS", "a", "b"],
        );
        assert!(o.silencioso, "/P é passivo: instala sem interface");
        assert!(o.relancar, "/R relança a app no fim");
        assert!(
            o.atualizacao,
            "/UPDATE é o que não recria o atalho da área de trabalho"
        );
        assert_eq!(o.args_da_app, vec!["a".to_string(), "b".to_string()]);
        assert!(matches!(o.modo, Modo::Instalar));
    }

    /// TUDO o que vem depois do `/ARGS` é da app — mesmo o que parece nosso.
    ///
    /// Se o `/ARGS` não parasse a leitura, um argumento da app com o mesmo nome de um nosso
    /// mudava o comportamento do instalador. `--apagar-dados` depois do `/ARGS` é o caso
    /// extremo, e é o que torna a regra visível.
    #[test]
    fn depois_do_args_nada_e_nosso() {
        let o = de(
            "Instalar-Bruma.exe",
            &["/ARGS", "--apagar-dados", "/UPDATE"],
        );
        assert!(!o.apagar_dados, "isso era para a app, não para nós");
        assert!(!o.atualizacao, "e isto também");
        assert_eq!(
            o.args_da_app,
            vec!["--apagar-dados".to_string(), "/UPDATE".to_string()]
        );
    }

    /// O NOME DO FICHEIRO É QUE DECIDE O PAPEL. Um binário, dois papéis.
    #[test]
    fn o_nome_do_exe_escolhe_o_papel() {
        assert!(matches!(de("uninstall.exe", &[]).modo, Modo::Desinstalar));
        // O Windows não distingue maiúsculas em nomes de ficheiro, e nós também não podemos.
        assert!(matches!(de("Uninstall.EXE", &[]).modo, Modo::Desinstalar));
        assert!(matches!(de("Instalar-Bruma.exe", &[]).modo, Modo::Instalar));
        // E o `--uninstall` chega para o mesmo, venha o exe com o nome que vier.
        assert!(matches!(
            de("Instalar-Bruma.exe", &["--uninstall"]).modo,
            Modo::Desinstalar
        ));
    }

    /// APAGAR A IDENTIDADE SÓ COM ESSE NOME EXACTO, e nunca por acidente.
    ///
    /// É a única opção irreversível que este programa tem. Um prefixo, um plural ou um erro de
    /// escrita não podem ligá-la — e o silêncio de um `_ =>` que aceita quase-acertos é
    /// exactamente como isso aconteceria.
    #[test]
    fn a_identidade_so_se_apaga_com_o_nome_exacto() {
        assert!(de("uninstall.exe", &["--apagar-dados"]).apagar_dados);
        for quase in [
            "--apagar-dado",
            "--apagar_dados",
            "--apagar",
            "-apagar-dados",
        ] {
            assert!(
                !de("uninstall.exe", &[quase]).apagar_dados,
                "«{quase}» não pode apagar a identidade de ninguém"
            );
        }
    }

    /// As duas formas de dizer a pasta, incluindo a que fala NSIS.
    #[test]
    fn as_duas_formas_de_dizer_a_pasta() {
        assert_eq!(
            de("Instalar-Bruma.exe", &["--dir=C:\\Bruma"]).dir,
            Some(PathBuf::from("C:\\Bruma"))
        );
        // Instalações antigas invocam o desinstalador assim.
        assert_eq!(
            de("uninstall.exe", &["_?=C:\\Bruma"]).dir,
            Some(PathBuf::from("C:\\Bruma"))
        );
        assert_eq!(de("Instalar-Bruma.exe", &[]).dir, None);
    }

    /// A pasta de dados que a pessoa escolheu à mão CONTA para o apagar (#183).
    ///
    /// Quem define o `BRUMA_DADOS` é precisamente quem mudou a pasta de sítio. Dizer-lhe que a
    /// identidade foi apagada sem sequer olhar para lá seria a mesma mentira do #183, com um
    /// passo a mais.
    #[test]
    fn o_bruma_dados_entra_nos_sitios_a_procurar() {
        let destino = PathBuf::from("C:\\Bruma");
        // Sem a variável: em teste, só a pasta ao lado do executável.
        let sem = sitios_dos_dados(&destino, true);
        assert_eq!(sem, vec![destino.join("dados")]);

        // O teste corre em paralelo com outros e a variável é global ao processo, mas
        // nenhum outro teste deste ficheiro lhe toca — e repõe-se a seguir.
        let antes = std::env::var_os("BRUMA_DADOS");
        std::env::set_var("BRUMA_DADOS", "C:\\outro-sitio");
        let com = sitios_dos_dados(&destino, true);
        match antes {
            Some(v) => std::env::set_var("BRUMA_DADOS", v),
            None => std::env::remove_var("BRUMA_DADOS"),
        }
        assert!(
            com.contains(&PathBuf::from("C:\\outro-sitio")),
            "a pasta escolhida à mão tem de ser procurada: {com:?}"
        );
    }
}
