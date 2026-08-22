//! Partilha de ecrã captada e codificada por nós, sem passar pelo `getDisplayMedia`.
//!
//! # Porquê não usar a webview para isto
//!
//! O `getDisplayMedia` funciona, mas traz duas coisas que não se resolvem por
//! configuração: o WebView2 desenha por cima da app a barra "está a partilhar uma janela"
//! — e não há API nem flag para a esconder, porque é o indicador de segurança dele — e o
//! codificador acaba por ser software, com a placa gráfica parada ao lado.
//!
//! Captando aqui, o WebView2 fica sem nada para anunciar e escolhemos nós o codificador.
//! O Spike 4 mediu: 55 fps a 3440×1440, a codificar custa ~8% do ritmo, e o codificador da
//! NVIDIA fica a 6% de uso. Ver `spikes/spike4-captura/README.md`.
//!
//! # Só se envia a quem está mesmo a ver
//!
//! A lista de espectadores vem da interface (quem carregou em "Assistir") e não de quem
//! está na sala. É a diferença entre mandar seis cópias e mandar uma: numa sala de seis
//! pessoas, quase sempre só uma ou duas têm a janela aberta. Enquanto ninguém estiver a
//! ver, os fragmentos são feitos e deitados fora — o que é intencional, porque parar e
//! recomeçar o codificador a cada espectador que chega custaria mais do que os deitar fora.

use anyhow::{anyhow, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Quem manda os pedaços para fora: um para a webview local, outro para a rede.
pub type Entrega = Arc<dyn Fn(&[u8]) + Send + Sync>;

/// O que está a acontecer agora. Uma partilha de cada vez, como no Discord.
#[derive(Default)]
pub struct Estado {
    parar: Option<Arc<AtomicBool>>,
    /// Chaves dos peers que carregaram em "Assistir".
    pub espectadores: Mutex<Vec<String>>,
}

impl Estado {
    pub fn a_partilhar(&self) -> bool {
        self.parar.is_some()
    }
}

#[cfg(windows)]
mod win {
    use super::{Entrega, Result};
    use anyhow::anyhow;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
    use windows_capture::frame::Frame;
    use windows_capture::graphics_capture_api::InternalCaptureControl;
    use windows_capture::monitor::Monitor;
    use windows_capture::settings::{
        ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
        MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
    };

    use crate::fmp4::Codificador;

    struct Sessao {
        codificador: Option<Codificador>,
        parar: Arc<AtomicBool>,
        /// Buffer reutilizado para tirar o enchimento do fim das linhas. Sem isto era uma
        /// alocação de 20 MB por frame, sessenta vezes por segundo.
        scratch: Vec<u8>,
    }

    // SAFETY: o `Sessao` guarda ponteiros COM, que em geral não atravessam threads. Aqui
    // não atravessam: o `start` cria a fila de despacho na thread que o chama, chama o
    // `new` nessa thread e entrega os frames nessa mesma thread. A exigência de `Send` vem
    // da assinatura do trait, não de haver travessia real. Se algum dia se passar ao
    // `start_free_threaded`, esta garantia cai.
    unsafe impl Send for Sessao {}

    type Flags = (u32, u32, u32, u32, Arc<AtomicBool>, Entrega);

    impl GraphicsCaptureApiHandler for Sessao {
        type Flags = Flags;
        type Error = Box<dyn std::error::Error + Send + Sync>;

        fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
            let (lar, alt, ls, aa, parar, entrega) = ctx.flags;
            let codificador = Codificador::novo(lar, alt, ls, aa, 60, 8_000_000, entrega)?;
            Ok(Self {
                codificador: Some(codificador),
                parar,
                scratch: Vec::new(),
            })
        }

        fn on_frame_arrived(
            &mut self,
            frame: &mut Frame,
            controlo: InternalCaptureControl,
        ) -> Result<(), Self::Error> {
            if self.parar.load(Ordering::Relaxed) {
                if let Some(c) = self.codificador.take() {
                    c.terminar()?;
                }
                controlo.stop();
                return Ok(());
            }
            let buf = frame.buffer()?;
            let bytes = buf.as_nopadding_buffer(&mut self.scratch);
            if let Some(c) = self.codificador.as_mut() {
                c.frame(bytes)?;
            }
            Ok(())
        }

        fn on_closed(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    /// Limita ao que faz sentido enviar. Um ecrã ultrawide inteiro não cabe no upload de
    /// ninguém. Par a par porque o H.264 trabalha em blocos e recusa dimensões ímpares.
    fn caber(lar: u32, alt: u32) -> (u32, u32) {
        let fator = (1920.0 / lar as f64).min(1080.0 / alt as f64).min(1.0);
        let par = |v: f64| ((v.round() as u32) / 2) * 2;
        (
            par(lar as f64 * fator).max(2),
            par(alt as f64 * fator).max(2),
        )
    }

    /// Arranca a captura numa thread própria. Devolve quando a captura já está a correr.
    pub fn arrancar(parar: Arc<AtomicBool>, entrega: Entrega) -> Result<(u32, u32)> {
        let monitor = Monitor::primary().map_err(|e| anyhow!("sem monitor: {e:?}"))?;
        let lar = monitor.width().map_err(|e| anyhow!("{e:?}"))?;
        let alt = monitor.height().map_err(|e| anyhow!("{e:?}"))?;
        let (ls, aa) = caber(lar, alt);

        std::thread::spawn(move || {
            let definicoes = Settings::new(
                monitor,
                CursorCaptureSettings::WithCursor,
                // A moldura amarela do Windows é o equivalente da barra do WebView2: se
                // não sair, trocava-se um aviso por outro.
                DrawBorderSettings::WithoutBorder,
                SecondaryWindowSettings::Default,
                MinimumUpdateIntervalSettings::Default,
                DirtyRegionSettings::Default,
                ColorFormat::Bgra8,
                (lar, alt, ls, aa, parar, entrega),
            );
            if let Err(e) = Sessao::start(definicoes) {
                eprintln!("[ecrã] a captura terminou: {e:?}");
            }
        });

        Ok((ls, aa))
    }
}

#[cfg(not(windows))]
mod win {
    use super::{Entrega, Result};
    use anyhow::anyhow;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    pub fn arrancar(_parar: Arc<AtomicBool>, _entrega: Entrega) -> Result<(u32, u32)> {
        Err(anyhow!(
            "a captura nativa por enquanto só existe no Windows"
        ))
    }
}

/// Começa a partilhar. Os pedaços vão para `entrega` assim que existem.
pub fn comecar(estado: &mut Estado, entrega: Entrega) -> Result<(u32, u32)> {
    if estado.a_partilhar() {
        return Err(anyhow!("já estás a partilhar"));
    }
    let parar = Arc::new(AtomicBool::new(false));
    let tamanho = win::arrancar(parar.clone(), entrega)?;
    estado.parar = Some(parar);
    Ok(tamanho)
}

/// Pede à captura para parar. Ela pára no frame seguinte — não se mata a thread a meio de
/// escrever, senão o último fragmento fica cortado.
pub fn parar(estado: &mut Estado) {
    if let Some(p) = estado.parar.take() {
        p.store(true, Ordering::Relaxed);
    }
    estado.espectadores.lock().unwrap().clear();
}
