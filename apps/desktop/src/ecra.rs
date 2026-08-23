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

/// O que se vai capturar: um monitor pelo índice, ou uma janela pelo handle.
///
/// O handle é só um número — reconstrói-se o `Window` dentro da thread da captura,
/// porque os tipos do capturador guardam ponteiros que não atravessam threads.
#[derive(Clone, Copy)]
pub enum Alvo {
    Ecra(usize),
    Janela(isize),
}

impl Alvo {
    /// `ecra:0` ou `janela:123456`, tal como o seletor os anuncia.
    pub fn analisar(texto: &str) -> Result<Self> {
        if let Some(n) = texto.strip_prefix("ecra:") {
            return Ok(Alvo::Ecra(n.parse()?));
        }
        if let Some(n) = texto.strip_prefix("janela:") {
            return Ok(Alvo::Janela(n.parse()?));
        }
        Err(anyhow!("fonte desconhecida: {texto}"))
    }
}

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
    use windows_capture::window::Window;

    use crate::fmp4::Codificador;

    /// O que o codificador tem para fazer. Vem do vídeo E do som, por um canal só.
    pub enum Trabalho {
        Video(Vec<u8>),
        Som(Vec<u8>, i64, i64),
    }

    struct Sessao {
        /// Para onde vão os frames. O codificador vive noutra thread — ver `arrancar`.
        canal: std::sync::mpsc::SyncSender<Trabalho>,
        parar: Arc<AtomicBool>,
        /// Buffer reutilizado para tirar o enchimento do fim das linhas. Sem isto era uma
        /// alocação de 20 MB por frame, sessenta vezes por segundo.
        scratch: Vec<u8>,
        /// O tamanho com que o codificador foi aberto. Uma JANELA muda de tamanho a
        /// meio — o codificador não. Ver `on_frame_arrived`.
        lar: u32,
        alt: u32,
        /// Tela persistente para onde se copiam frames de tamanho diferente do inicial.
        tela: Vec<u8>,
        /// Medição: entregues pelo Windows, enviados ao codificador, e largados por o
        /// codificador estar atrasado.
        recebidos: u64,
        enviados: u64,
        largados: u64,
        inicio: std::time::Instant,
        fps: u32,
    }

    type Flags = (
        std::sync::mpsc::SyncSender<Trabalho>,
        u32,
        u32,
        Arc<AtomicBool>,
        u32,
    );

    impl GraphicsCaptureApiHandler for Sessao {
        type Flags = Flags;
        type Error = Box<dyn std::error::Error + Send + Sync>;

        fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
            let (canal, lar, alt, parar, fps) = ctx.flags;
            Ok(Self {
                canal,
                parar,
                scratch: Vec::new(),
                lar,
                alt,
                tela: Vec::new(),
                recebidos: 0,
                enviados: 0,
                largados: 0,
                inicio: std::time::Instant::now(),
                fps,
            })
        }

        fn on_frame_arrived(
            &mut self,
            frame: &mut Frame,
            controlo: InternalCaptureControl,
        ) -> Result<(), Self::Error> {
            if self.parar.load(Ordering::Relaxed) {
                // O veredicto da CAPTURA. O relógio da média fica do lado do codificador,
                // que o imprime quando fecha — aqui já não se lhe pode perguntar nada.
                let s = self.inicio.elapsed().as_secs_f64().max(0.001);
                eprintln!(
                    "[ecrã] {:.1}s: {} entregues ({:.1}/s), {} enviados, {} largados, \
                     pedido {} ips",
                    s,
                    self.recebidos,
                    self.recebidos as f64 / s,
                    self.enviados,
                    self.largados,
                    self.fps,
                );
                controlo.stop();
                return Ok(());
            }
            self.recebidos += 1;
            let (f_lar, f_alt) = (frame.width(), frame.height());
            let buf = frame.buffer()?;
            let bytes = buf.as_nopadding_buffer(&mut self.scratch);

            {
                let pronto: Vec<u8> = if f_lar == self.lar && f_alt == self.alt {
                    bytes.to_vec()
                } else {
                    // A janela mudou de tamanho e o codificador não muda com ela — o MP4
                    // declara as dimensões no cabeçalho, uma vez. Copia-se o que couber
                    // para uma tela do tamanho original: encolher deixa margem preta,
                    // crescer corta. Imperfeito e assumido — a alternativa era a
                    // transmissão MORRER ao primeiro redimensionar, que é pior.
                    let destino = (self.lar * self.alt * 4) as usize;
                    if self.tela.len() != destino {
                        self.tela = vec![0u8; destino];
                    }
                    let linhas = f_alt.min(self.alt) as usize;
                    let largura = (f_lar.min(self.lar) * 4) as usize;
                    for y in 0..linhas {
                        let de = y * (f_lar * 4) as usize;
                        let para = y * (self.lar * 4) as usize;
                        self.tela[para..para + largura].copy_from_slice(&bytes[de..de + largura]);
                    }
                    self.tela.clone()
                };
                // `try_send` e não `send`: se o codificador está atrasado, LARGA-SE o frame
                // em vez de segurar a captura à espera dele. Numa transmissão ao vivo,
                // chegar tarde é pior do que faltar — e segurar aqui atrasaria também o
                // som, que vai pelo mesmo canal.
                match self.canal.try_send(Trabalho::Video(pronto)) {
                    Ok(()) => self.enviados += 1,
                    Err(_) => self.largados += 1,
                }
            }
            Ok(())
        }

        fn on_closed(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    /// Limita à altura escolhida, mantendo a proporção. `0` significa nativa — vai como
    /// está. Par a par porque o H.264 trabalha em blocos e recusa dimensões ímpares.
    fn caber(lar: u32, alt: u32, max_alt: u32) -> (u32, u32) {
        let fator = if max_alt == 0 {
            1.0
        } else {
            (max_alt as f64 / alt as f64).min(1.0)
        };
        let par = |v: f64| ((v.round() as u32) / 2) * 2;
        (
            par(lar as f64 * fator).max(2),
            par(alt as f64 * fator).max(2),
        )
    }

    /// O débito que uma escolha de qualidade merece: ~0,065 bits por pixel por imagem,
    /// que a 1080p60 dá os ~8 Mbps já medidos no Spike 4. Com teto, porque o upload de
    /// casa é finito — e com chão, porque abaixo disso o H.264 vira aguarela.
    fn debito(lar: u32, alt: u32, fps: u32) -> u32 {
        let bruto = (lar as u64 * alt as u64 * fps as u64) as f64 * 0.065;
        (bruto as u32).clamp(2_500_000, 20_000_000)
    }

    fn tamanho_do_alvo(alvo: super::Alvo) -> Result<(u32, u32)> {
        match alvo {
            super::Alvo::Ecra(i) => {
                let m = Monitor::from_index(i).map_err(|e| anyhow!("sem esse ecrã: {e:?}"))?;
                Ok((
                    m.width().map_err(|e| anyhow!("{e:?}"))?,
                    m.height().map_err(|e| anyhow!("{e:?}"))?,
                ))
            }
            super::Alvo::Janela(h) => {
                let j = Window::from_raw_hwnd(h as *mut std::ffi::c_void);
                if !j.is_valid() {
                    return Err(anyhow!("essa janela já fechou"));
                }
                Ok((
                    j.width().map_err(|e| anyhow!("{e:?}"))? as u32,
                    j.height().map_err(|e| anyhow!("{e:?}"))? as u32,
                ))
            }
        }
    }

    /// Arranca a captura do alvo escolhido numa thread própria.
    pub fn arrancar(
        alvo: super::Alvo,
        qualidade: super::Qualidade,
        parar: Arc<AtomicBool>,
        entrega: Entrega,
    ) -> Result<(u32, u32)> {
        let (lar, alt) = tamanho_do_alvo(alvo)?;
        let (ls, aa) = caber(lar, alt, qualidade.max_altura);
        let fps = qualidade.fps.clamp(15, 60);
        let bitrate = if qualidade.debito > 0 {
            qualidade.debito.clamp(1_000_000, 25_000_000)
        } else {
            debito(ls, aa, fps)
        };

        // O formato do som LÊ-SE antes de o codificador nascer, porque ele precisa do ritmo
        // e dos canais para declarar a faixa. Sem dispositivo, a partilha segue muda — e
        // di-lo, em vez de morrer.
        let formato_som = if qualidade.com_som {
            match crate::som::formato() {
                Ok(f) => Some(f),
                Err(e) => {
                    eprintln!("[som] sem dispositivo de saída; a partilha vai muda: {e:?}");
                    None
                }
            }
        } else {
            None
        };

        // Uma fila curta de propósito: com quatro lugares, o atraso máximo que o codificador
        // pode acumular é de quatro frames. Mais fundo seria guardar imagens que, quando
        // saíssem, já não interessavam a ninguém.
        let (envia, recebe) = std::sync::mpsc::sync_channel::<Trabalho>(4);
        let origem = std::time::Instant::now();

        // A thread do codificador. É a ÚNICA que fala com o Media Foundation: o vídeo e o
        // som chegam-lhe pelo canal, e assim nunca há dois lados a escrever no mesmo sink —
        // que é precisamente onde este tipo de código costuma partir-se de forma aleatória.
        std::thread::spawn(move || {
            let mut c = match Codificador::novo(
                lar,
                alt,
                ls,
                aa,
                fps,
                bitrate,
                formato_som,
                origem,
                entrega,
            ) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("[ecrã] o codificador não abriu: {e:?}");
                    return;
                }
            };
            let mut sons = 0u64;
            while let Ok(t) = recebe.recv() {
                let r = match t {
                    Trabalho::Video(v) => c.frame(&v),
                    Trabalho::Som(pcm, i, d) => {
                        sons += 1;
                        c.som(&pcm, i, d)
                    }
                };
                if let Err(e) = r {
                    eprintln!("[ecrã] o codificador queixou-se: {e:?}");
                    break;
                }
            }
            let media = c.relogio_media();
            let s = origem.elapsed().as_secs_f64().max(0.001);
            eprintln!(
                "[ecrã] fim: média {media:.1}s para {s:.1}s de parede (x{:.2}), \
                 {sons} bocados de som",
                media / s
            );
            if let Err(e) = c.terminar() {
                eprintln!("[ecrã] ao fechar: {e:?}");
            }
        });

        // E o som, se houver. Vai pelo mesmo canal, ancorado ao mesmo relógio.
        if formato_som.is_some() {
            let envia_som = envia.clone();
            crate::som::arrancar(parar.clone(), origem, move |b| {
                let _ = envia_som.try_send(Trabalho::Som(b.pcm, b.instante, b.duracao));
            });
        }

        std::thread::spawn(move || {
            // O item de captura constrói-se AQUI dentro: os tipos do capturador guardam
            // ponteiros COM que não atravessam threads; o Alvo é só números e atravessa.
            let flags = (envia, lar, alt, parar, fps);
            // Função e não closure: os dois ramos passam tipos diferentes (Monitor e
            // Window) e uma closure fixa-se no primeiro que vê.
            fn definicoes<T: TryInto<windows_capture::settings::GraphicsCaptureItemType>>(
                item: T,
                flags: Flags,
            ) -> Settings<Flags, T> {
                Settings::new(
                    item,
                    CursorCaptureSettings::WithCursor,
                    // A moldura amarela do Windows é o equivalente da barra do WebView2:
                    // se não sair, trocava-se um aviso por outro.
                    DrawBorderSettings::WithoutBorder,
                    SecondaryWindowSettings::Default,
                    // O ritmo pedido travado na ORIGEM: o Windows nem chega a capturar o
                    // frame que viria cedo demais. Travar aqui poupa a captura inteira —
                    // travar no nosso lado só pouparia a codificação, depois de a placa
                    // gráfica já ter feito o trabalho.
                    MinimumUpdateIntervalSettings::Custom(std::time::Duration::from_secs_f64(
                        1.0 / flags.4.max(1) as f64,
                    )),
                    DirtyRegionSettings::Default,
                    ColorFormat::Bgra8,
                    flags,
                )
            }
            let resultado = match alvo {
                super::Alvo::Ecra(i) => match Monitor::from_index(i) {
                    Ok(m) => Sessao::start(definicoes(m, flags)),
                    Err(e) => {
                        eprintln!("[ecrã] o ecrã desapareceu: {e:?}");
                        return;
                    }
                },
                super::Alvo::Janela(h) => Sessao::start(definicoes(
                    Window::from_raw_hwnd(h as *mut std::ffi::c_void),
                    flags,
                )),
            };
            if let Err(e) = resultado {
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

    pub fn arrancar(
        _alvo: super::Alvo,
        _q: super::Qualidade,
        _parar: Arc<AtomicBool>,
        _entrega: Entrega,
    ) -> Result<(u32, u32)> {
        Err(anyhow!(
            "a captura nativa por enquanto só existe no Windows"
        ))
    }
}

/// Começa a partilhar. Os pedaços vão para `entrega` assim que existem.
/// A qualidade que a pessoa escolheu no seletor.
#[derive(Clone, Copy)]
pub struct Qualidade {
    /// Altura máxima em pixels; 0 = nativa.
    pub max_altura: u32,
    pub fps: u32,
    /// Se o som que sai das colunas segue com a imagem.
    pub com_som: bool,
    /// Débito em bits por segundo; 0 = calcular a partir da resolução e do ritmo.
    /// Existe porque o upload de casa manda mais do que qualquer fórmula: quem tem
    /// 5 Mbps de subida quer poder dizer "gasta 4 e nem mais um".
    pub debito: u32,
}

pub fn comecar(
    estado: &mut Estado,
    alvo: Alvo,
    qualidade: Qualidade,
    entrega: Entrega,
) -> Result<(u32, u32)> {
    if estado.a_partilhar() {
        return Err(anyhow!("já estás a partilhar"));
    }
    let parar = Arc::new(AtomicBool::new(false));
    let tamanho = win::arrancar(alvo, qualidade, parar.clone(), entrega)?;
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
