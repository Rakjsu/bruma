//! Deteção do que o utilizador tem aberto, para a barra de "estás a jogar X".
//!
//! Não há base de dados de jogos nem lista mantida à mão: percorrem-se as janelas visíveis,
//! descartam-se as que são obviamente ferramentas, e fica a que tiver mais cara de aplicação
//! em primeiro plano. É um palpite, e é apresentado como tal — a app diz "tens isto aberto",
//! não "detetámos o teu jogo".

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Janela {
    pub titulo: String,
    pub processo: String,
    /// Quanto da área do ecrã principal esta janela ocupa. Um jogo tende a ocupar tudo.
    pub cobertura: f32,
}

/// Processos que nunca são "o jogo": o próprio Bruma, o explorador, browsers, editores e
/// utilitários. Comparado em minúsculas e sem `.exe`.
const NUNCA: &[&str] = &[
    "bruma",
    "explorer",
    "searchhost",
    "shellexperiencehost",
    "startmenuexperiencehost",
    "textinputhost",
    "applicationframehost",
    "systemsettings",
    "chrome",
    "msedge",
    "msedgewebview2",
    "firefox",
    "opera",
    "brave",
    "code",
    "devenv",
    "rider64",
    "idea64",
    "windowsterminal",
    "cmd",
    "powershell",
    "pwsh",
    "conhost",
    "mintty",
    "notepad",
    "taskmgr",
    "discord",
    "spotify",
    "whatsapp",
    "obs64",
    "steamwebhelper",
    "claude",
    "cursor",
];

#[cfg(windows)]
mod win {
    use super::{Janela, NUNCA};
    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM, MAX_PATH, RECT};
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetForegroundWindow, GetSystemMetrics, GetWindowRect, GetWindowTextW,
        GetWindowThreadProcessId, IsWindowVisible, SM_CXSCREEN, SM_CYSCREEN,
    };

    struct Recolha {
        janelas: Vec<Janela>,
        frente: isize,
    }

    unsafe extern "system" fn visitar(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let recolha = &mut *(lparam.0 as *mut Recolha);

        if !IsWindowVisible(hwnd).as_bool() {
            return true.into();
        }

        let mut buf = [0u16; 512];
        let n = GetWindowTextW(hwnd, &mut buf);
        if n == 0 {
            return true.into();
        }
        let titulo = String::from_utf16_lossy(&buf[..n as usize]);
        if titulo.trim().is_empty() {
            return true.into();
        }

        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let processo = nome_do_processo(pid).unwrap_or_default();
        let curto = processo
            .rsplit(['\\', '/'])
            .next()
            .unwrap_or("")
            .trim_end_matches(".exe")
            .to_lowercase();
        if curto.is_empty() || NUNCA.contains(&curto.as_str()) {
            return true.into();
        }

        // Quanto do ecrã principal ocupa. Serve para separar um jogo de uma janelinha.
        let mut r = RECT::default();
        let cobertura = if GetWindowRect(hwnd, &mut r).is_ok() {
            let (largura, altura) = ((r.right - r.left) as f32, (r.bottom - r.top) as f32);
            let (ew, eh) = (
                GetSystemMetrics(SM_CXSCREEN) as f32,
                GetSystemMetrics(SM_CYSCREEN) as f32,
            );
            if ew > 0.0 && eh > 0.0 {
                ((largura * altura) / (ew * eh)).clamp(0.0, 1.0)
            } else {
                0.0
            }
        } else {
            0.0
        };
        // Janelas minúsculas não interessam a ninguém.
        if cobertura < 0.12 {
            return true.into();
        }

        // A janela em primeiro plano ganha vantagem: é quase de certeza o que a pessoa usa.
        let bonus = if hwnd.0 as isize == recolha.frente {
            1.0
        } else {
            0.0
        };

        recolha.janelas.push(Janela {
            titulo,
            processo: curto,
            cobertura: cobertura + bonus,
        });
        true.into()
    }

    fn nome_do_processo(pid: u32) -> Option<String> {
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let mut buf = [0u16; MAX_PATH as usize];
            let mut tamanho = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(
                h,
                PROCESS_NAME_WIN32,
                windows::core::PWSTR(buf.as_mut_ptr()),
                &mut tamanho,
            );
            let _ = windows::Win32::Foundation::CloseHandle(h);
            if ok.is_err() {
                return None;
            }
            Some(String::from_utf16_lossy(&buf[..tamanho as usize]))
        }
    }

    pub fn em_execucao() -> Option<Janela> {
        let mut recolha = Recolha {
            janelas: Vec::new(),
            frente: unsafe { GetForegroundWindow().0 as isize },
        };
        unsafe {
            let _ = EnumWindows(Some(visitar), LPARAM(&mut recolha as *mut Recolha as isize));
        }
        recolha.janelas.into_iter().max_by(|a, b| {
            a.cobertura
                .partial_cmp(&b.cobertura)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    }
}

#[cfg(not(windows))]
mod win {
    use super::Janela;
    pub fn em_execucao() -> Option<Janela> {
        None
    }
}

/// O que a pessoa tem aberto neste momento, se der para adivinhar.
#[tauri::command]
pub fn jogo_em_execucao() -> Option<Janela> {
    win::em_execucao()
}
