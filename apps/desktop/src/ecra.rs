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

/// Quem é avisado quando a captura morre depois de ter dito que ia começar.
///
/// # Porque é que isto teve de existir
///
/// `arrancar` devolve `Ok` assim que sabe o tamanho do alvo — e a captura só arranca
/// mesmo numa thread, mais tarde. Se ela falhar aí (uma definição que este Windows não
/// tem, o ecrã a desaparecer, o codificador a recusar), o erro ia parar a um `eprintln!`
/// numa consola que, em release, NÃO EXISTE: o binário é compilado com
/// `windows_subsystem = "windows"`. A interface ficava a dizer "estás a partilhar" para
/// sempre, sem imagem e sem explicação.
///
/// Uma falha que ninguém vê é pior do que uma falha ruidosa.
pub type Queixa = Arc<dyn Fn(String) + Send + Sync>;

/// Irmão da `Queixa`, para o que **não** impede a partilha mas a pessoa tem de saber.
///
/// A diferença é deliberada: a queixa apaga o estado de "estás a partilhar", o aviso
/// deixa-o de pé. Usar a queixa para um aviso desligava a transmissão por causa de uma
/// imperfeição; usar um `eprintln!` deixava a pessoa a mandar a voz de toda a gente de
/// volta sem nunca o saber.
pub type Aviso = Arc<dyn Fn(String) + Send + Sync>;

/// O que está a acontecer agora. Uma partilha de cada vez, como no Discord.
#[derive(Default)]
pub struct Estado {
    parar: Option<Arc<AtomicBool>>,
    /// Chaves dos peers que carregaram em "Assistir".
    pub espectadores: Mutex<Vec<String>>,
}

impl Estado {
    /// Há uma partilha VIVA — e não só um sinal guardado.
    ///
    /// Era `parar.is_some()`. Com a queixa a pôr o sinal a `true` (#40), uma partilha que
    /// morreu sozinha continuava a responder «já estás a partilhar» a quem tentasse outra,
    /// até a interface fazer a limpeza de volta. O sinal posto É a partilha a acabar; olha-se
    /// ao valor e não à existência.
    pub fn a_partilhar(&self) -> bool {
        matches!(&self.parar, Some(p) if !p.load(Ordering::Relaxed))
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
        canal: std::sync::mpsc::Sender<Trabalho>,
        /// Quantos frames de vídeo estão à espera. É este contador — e não a fundura do
        /// canal — que decide quando se larga um frame; ver `arrancar`.
        na_fila: Arc<std::sync::atomic::AtomicUsize>,
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
        /// As dimensões do último frame que passou pela tela (#167). Quando mudam, a tela
        /// limpa-se: sem isto, uma janela que encolhe deixava na margem uma orla do
        /// conteúdo antigo, porque só se copiava por cima do que cabia.
        tela_de: (u32, u32),
        /// Medição: entregues pelo Windows, enviados ao codificador, e largados por o
        /// codificador estar atrasado.
        recebidos: u64,
        enviados: u64,
        largados: u64,
        inicio: std::time::Instant,
        fps: u32,
        /// Diagnóstico dos intervalos entre frames: numa captura ao vivo, um intervalo
        /// longo é uma imagem congelada do lado de quem vê.
        ultimo_frame: Option<std::time::Instant>,
        maior_intervalo: u64,
        longos: u64,
    }

    /// O que a captura precisa de saber. Struct e não tuplo: quando isto era
    /// `(Sender, u32, u32, Arc, u32)`, acrescentar um campo ao meio calou um `flags.4` que
    /// passou a apontar para outra coisa — e só o tipo o apanhou. Com nomes, não há índice
    /// para se deslocar.
    struct Flags {
        canal: std::sync::mpsc::Sender<Trabalho>,
        na_fila: Arc<std::sync::atomic::AtomicUsize>,
        lar: u32,
        alt: u32,
        parar: Arc<AtomicBool>,
        fps: u32,
    }

    impl Sessao {
        /// O veredicto da captura, para o registo. Chamado pelo vigia depois do `stop()`,
        /// com a thread da captura já junta — é o único sítio de onde se consegue olhar
        /// para a `Sessao` tenha ela acabado bem ou mal.
        fn resumo(&self) -> String {
            let s = self.inicio.elapsed().as_secs_f64().max(0.001);
            format!(
                "[ecrã] {:.1}s: {} entregues ({:.1}/s), {} enviados, {} largados, pedido \
                 {} ips | intervalos: maior {} ms, {} acima de 400 ms",
                s,
                self.recebidos,
                self.recebidos as f64 / s,
                self.enviados,
                self.largados,
                self.fps,
                self.maior_intervalo,
                self.longos
            )
        }
    }

    impl GraphicsCaptureApiHandler for Sessao {
        type Flags = Flags;
        type Error = Box<dyn std::error::Error + Send + Sync>;

        fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
            let Flags {
                canal,
                na_fila,
                lar,
                alt,
                parar,
                fps,
            } = ctx.flags;
            Ok(Self {
                canal,
                na_fila,
                parar,
                scratch: Vec::new(),
                lar,
                alt,
                tela: Vec::new(),
                tela_de: (0, 0),
                recebidos: 0,
                enviados: 0,
                largados: 0,
                inicio: std::time::Instant::now(),
                fps,
                ultimo_frame: None,
                maior_intervalo: 0,
                longos: 0,
            })
        }

        fn on_frame_arrived(
            &mut self,
            frame: &mut Frame,
            controlo: InternalCaptureControl,
        ) -> Result<(), Self::Error> {
            // `BRUMA_SO_VIGIA=1` finge um ecrã parado: o tratador deixa de reagir ao sinal
            // e a paragem passa a depender SÓ do vigia. É a única forma de correr aqui o
            // caminho que, lá fora, acontece com uma janela minimizada.
            if self.parar.load(Ordering::Relaxed) && !crate::bandeiras::so_vigia() {
                // O veredicto da CAPTURA imprime-se no VIGIA, depois do `stop()`, e não
                // aqui. Aqui só corre quando o Windows entrega um frame — e uma captura
                // que morreu sozinha, ou um ecrã parado com `BRUMA_SO_VIGIA`, nunca mais
                // entrega nenhum. Os números da sessão que mais interessam eram
                // precisamente os da sessão que acabou mal, e esses nunca saíam.
                controlo.stop();
                return Ok(());
            }
            // `BRUMA_ITEM_FECHA_AOS=N` finge que o item se fechou ao fim de N segundos:
            // devolve-se `Err` daqui, que o crate trata exactamente como o `Err` do
            // `on_closed` — guarda-o e devolve-o no `stop()`. Não é o evento `Closed` a
            // sério (esse só o Windows o dispara), mas é o mesmo caminho de saída.
            if let Some(aos) = crate::bandeiras::item_fecha_aos() {
                if self.inicio.elapsed().as_secs() >= aos {
                    return Err("a janela ou o ecrã que estavas a partilhar desapareceu \
                                (BRUMA_ITEM_FECHA_AOS)"
                        .into());
                }
            }
            self.recebidos += 1;
            {
                let agora = std::time::Instant::now();
                if let Some(ant) = self.ultimo_frame {
                    let ms = agora.duration_since(ant).as_millis() as u64;
                    self.maior_intervalo = self.maior_intervalo.max(ms);
                    if ms > 400 {
                        self.longos += 1;
                    }
                }
                self.ultimo_frame = Some(agora);
            }
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
                    compor(
                        &mut self.tela,
                        &mut self.tela_de,
                        (self.lar, self.alt),
                        bytes,
                        (f_lar, f_alt),
                    );
                    self.tela.clone()
                };
                // Larga-se o frame quando o codificador está atrasado: numa transmissão ao
                // vivo, chegar tarde é pior do que faltar.
                //
                // Mas o limite conta SÓ os frames de vídeo, e não o que estiver no canal.
                // Antes o canal era partilhado com o som e tinha quatro lugares para os
                // dois: um pico de vídeo enchia-o e o `try_send` do som falhava — e um
                // bocado de som perdido não é como um frame perdido. O vídeo largado
                // apanha-se no frame seguinte; o som largado é um buraco que fica, e os
                // buracos SOMAM-SE: ao fim de uns minutos a imagem e o som já não estão
                // juntos. Agora o som nunca é largado, e o vídeo tem o seu próprio teto.
                if self.na_fila.load(Ordering::Relaxed) >= 4 {
                    self.largados += 1;
                } else {
                    self.na_fila.fetch_add(1, Ordering::Relaxed);
                    match self.canal.send(Trabalho::Video(pronto)) {
                        Ok(()) => self.enviados += 1,
                        Err(_) => {
                            self.na_fila.fetch_sub(1, Ordering::Relaxed);
                            self.largados += 1;
                        }
                    }
                }
            }
            Ok(())
        }

        /// O item fechou-se por baixo de nós: a janela fechou, o monitor foi desligado, o
        /// driver foi reposto (#39).
        ///
        /// Devolvia `Ok(())` e não fazia nada. A thread da captura acabava, o vigia saía
        /// pelo `is_finished()` e chamava `stop()` — sem uma única queixa. A interface
        /// ficava a dizer «estás a partilhar» para sempre, com o botão aceso e sem imagem.
        ///
        /// Um `Err` daqui é guardado pelo crate e devolvido pelo `stop()` do vigia como
        /// `FrameHandlerError` — é esse o caminho pelo qual o texto chega à pessoa.
        fn on_closed(&mut self) -> Result<(), Self::Error> {
            Err("a janela ou o ecrã que estavas a partilhar desapareceu".into())
        }
    }

    /// Compõe um frame de tamanho diferente do inicial na tela do tamanho inicial (#167).
    ///
    /// Encolher deixa margem preta, crescer corta. Imperfeito e assumido — a alternativa
    /// era a transmissão MORRER ao primeiro redimensionar, que é pior.
    ///
    /// A TELA LIMPA-SE QUANDO O TAMANHO MUDA. Era alocada uma vez com zeros e nunca mais
    /// tocada fora do rectângulo que se copiava: uma janela que encolhia de 1200 para 800
    /// de largura deixava 400 pixels de conteúdo velho em cada linha, e quem via ficava com
    /// uma orla do que a janela mostrava há um minuto. Um `fill` de 20 MB só na transição —
    /// e a transição é um arrasto de borda, onde alguns ms não se notam.
    ///
    /// Função e não código inline na tratadora: uma orla que fica é exactamente o género
    /// de defeito que ninguém vê a olhar, e assim há um teste que a vê.
    pub(super) fn compor(
        tela: &mut Vec<u8>,
        tela_de: &mut (u32, u32),
        (lar, alt): (u32, u32),
        bytes: &[u8],
        (f_lar, f_alt): (u32, u32),
    ) {
        let destino = (lar * alt * 4) as usize;
        if tela.len() != destino {
            *tela = vec![0u8; destino];
        }
        if *tela_de != (f_lar, f_alt) {
            tela.fill(0);
            *tela_de = (f_lar, f_alt);
        }
        let linhas = f_alt.min(alt) as usize;
        let largura = (f_lar.min(lar) * 4) as usize;
        for y in 0..linhas {
            let de = y * (f_lar * 4) as usize;
            let para = y * (lar * 4) as usize;
            tela[para..para + largura].copy_from_slice(&bytes[de..de + largura]);
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

    /// O cursor, se esta versão do Windows deixar escolher.
    ///
    /// Como o travão de ritmo e a moldura: pedir uma definição que o sistema não tem não é
    /// ignorado — é recusado, e leva a captura INTEIRA com ele.
    fn cursor() -> CursorCaptureSettings {
        use windows_capture::graphics_capture_api::GraphicsCaptureApi;
        static HA: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *HA.get_or_init(|| GraphicsCaptureApi::is_cursor_settings_supported().unwrap_or(false)) {
            CursorCaptureSettings::WithCursor
        } else {
            CursorCaptureSettings::Default
        }
    }

    /// A moldura amarela, se esta versão do Windows deixar tirá-la.
    ///
    /// # A avaria que estava aqui desde sempre
    ///
    /// `SetIsBorderRequired` chegou no contrato 12 (Windows 11 21H2 / Server 2022). Em
    /// Windows 10 não existe — e o `windows-capture` responde a um pedido impossível com
    /// `BorderConfigUnsupported`, não com um encolher de ombros. Ou seja: a partilha de
    /// ecrã do Bruma NUNCA funcionou em Windows 10, e ninguém o podia saber, porque em
    /// release não há consola onde ler o erro.
    ///
    /// Uma moldura amarela à volta do que se partilha é um preço pequeno. Ficar sem
    /// partilha nenhuma não é.
    fn moldura() -> DrawBorderSettings {
        use windows_capture::graphics_capture_api::GraphicsCaptureApi;
        static HA: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *HA.get_or_init(|| {
            let r = GraphicsCaptureApi::is_border_settings_supported().unwrap_or(false);
            if !r {
                eprintln!("[ecrã] este Windows não deixa tirar a moldura; vai com ela");
            }
            r
        }) {
            DrawBorderSettings::WithoutBorder
        } else {
            DrawBorderSettings::Default
        }
    }

    /// O travão de ritmo, se esta versão do Windows o tiver.
    ///
    /// A resposta não muda enquanto a app viver, e a pergunta envolve o `ApiInformation` do
    /// WinRT — pergunta-se uma vez. Quando não há, devolve-se `Default` e o ritmo passa a
    /// ser travado onde sempre foi possível travá-lo: no relógio de cada amostra, que já é
    /// real e não depende disto para o vídeo sair com a duração certa.
    fn intervalo_minimo(fps: u32) -> MinimumUpdateIntervalSettings {
        use windows_capture::graphics_capture_api::GraphicsCaptureApi;
        static HA: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let ha = *HA.get_or_init(|| {
            // `BRUMA_SEM_TRAVAO=1` finge um Windows sem esta API. Existe porque o ramo do
            // Windows antigo NUNCA corre na máquina onde isto se escreve, e um ramo que
            // nunca corre é um ramo por verificar — foi assim que este bug entrou.
            if crate::bandeiras::sem_travao() {
                eprintln!("[ecrã] a fingir um Windows sem travão de ritmo, a pedido");
                return false;
            }
            let r = GraphicsCaptureApi::is_minimum_update_interval_supported().unwrap_or(false);
            if !r {
                eprintln!(
 "[ecrã] este Windows não trava o ritmo na origem; a captura segue sem esse travão (o vídeo sai à mesma com a duração certa)"
                );
            }
            r
        });
        if ha {
            MinimumUpdateIntervalSettings::Custom(std::time::Duration::from_secs_f64(
                1.0 / fps.max(1) as f64,
            ))
        } else {
            MinimumUpdateIntervalSettings::Default
        }
    }

    /// Arranca a captura do alvo escolhido numa thread própria.
    pub fn arrancar(
        alvo: super::Alvo,
        qualidade: super::Qualidade,
        parar: Arc<AtomicBool>,
        entrega: Entrega,
        queixa: super::Queixa,
        aviso: super::Aviso,
    ) -> Result<(u32, u32)> {
        // A QUEIXA PÁRA TUDO O QUE ESTA FUNÇÃO LANÇOU (#40).
        //
        // Havia cinco sítios a queixar-se — o codificador que não abre, a morte pedida
        // para o teste, o Media Foundation que se queixa a meio, o ecrã que desapareceu ao
        // construir o item, a captura que não arrancou — e nenhum deles punha o `parar` a
        // `true`. Quem o punha era a INTERFACE, ao receber `partilha-falhou` e chamar
        // `parar_de_partilhar` de volta: um salto de ida e volta pela webview para
        // desligar uma thread que está a meio metro daqui.
        //
        // Enquanto esse salto não chegava — ou se não chegasse, porque a interface estava
        // presa ou porque o evento se perdeu — o laço do WASAPI continuava em
        // `while !parar.load()` a captar o som do sistema e a mandá-lo para um
        // codificador que já tinha morrido. Com a imagem morta, o som de quem partilha
        // continuava a sair da máquina.
        //
        // O embrulho é um só, à entrada, e todos os sítios o herdam — incluindo a queixa
        // nova do vigia (#39). Escrevê-lo em cada um era garantir que o próximo sítio a
        // queixar-se ficava outra vez de fora, que foi exactamente como o terceiro deles
        // escapou da primeira vez (ver o comentário no laço do codificador).
        //
        // `swap` e não `store`: se o sinal JÁ estava posto, a paragem foi pedida e o que
        // vem a seguir não é uma avaria — é a consequência de parar. Não se queixa do que
        // se pediu.
        let queixa: super::Queixa = {
            let p = parar.clone();
            let q = queixa;
            Arc::new(move |texto: String| {
                if !p.swap(true, Ordering::Relaxed) {
                    q(texto);
                }
            })
        };

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

        // O canal não tem fundo, mas o VÍDEO tem: `na_fila` conta os frames à espera e o
        // tratador larga-os acima de quatro. Assim o atraso de imagem continua limitado a
        // quatro frames, e o som — que é pequeno e cuja perda não se recupera — passa
        // sempre. Ver o comentário no `on_frame_arrived`.
        let (envia, recebe) = std::sync::mpsc::channel::<Trabalho>();
        let na_fila = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let origem = std::time::Instant::now();

        // A thread do codificador. É a ÚNICA que fala com o Media Foundation: o vídeo e o
        // som chegam-lhe pelo canal, e assim nunca há dois lados a escrever no mesmo sink —
        // que é precisamente onde este tipo de código costuma partir-se de forma aleatória.
        let na_fila_codificador = na_fila.clone();
        let queixa_codificador = queixa.clone();
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
                    queixa_codificador(format!("o codificador de vídeo não abriu: {e}"));
                    return;
                }
            };
            let mut sons = 0u64;
            // `BRUMA_CODIFICADOR_MORRE=20` faz o codificador desistir ao vigésimo frame.
            // É a única forma de correr aqui o caminho da morte a meio — o mesmo motivo das
            // outras bandeiras: um ramo que nunca corre é um ramo por verificar.
            let morre_ao: u64 = crate::bandeiras::codificador_morre_ao().unwrap_or(0);
            let mut feitos = 0u64;
            while let Ok(t) = recebe.recv() {
                feitos += 1;
                if morre_ao > 0 && feitos >= morre_ao {
                    queixa_codificador(
                        "o codificador de vídeo parou a meio: morte pedida para o teste".into(),
                    );
                    break;
                }
                let r = match t {
                    Trabalho::Video(v) => {
                        na_fila_codificador.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                        c.frame(&v)
                    }
                    Trabalho::Som(pcm, i, d) => {
                        sons += 1;
                        c.som(&pcm, i, d)
                    }
                };
                if let Err(e) = r {
                    eprintln!("[ecrã] o codificador queixou-se: {e:?}");
                    // A queixa TEM de subir. Quando esta thread morre, o emissor cai e o
                    // `send` do tratador passa a falhar em silêncio, a somar `largados`
                    // para sempre: a interface continuava a dizer "estás a partilhar" e
                    // quem assistia via a imagem congelada, sem um único sinal.
                    //
                    // Os outros dois caminhos de falha desta função já chamavam `queixa` —
                    // foi este que ficou de fora quando o tipo foi criado. A lição fica
                    // escrita porque se repetiu: quando se inventa um mecanismo para tornar
                    // uma falha visível, é preciso aplicá-lo a TODOS os sítios que falham
                    // dessa maneira, e não só ao que motivou a invenção.
                    queixa_codificador(format!("o codificador de vídeo parou a meio: {e}"));
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
        if let Some(f) = formato_som {
            // O aviso do eco NÃO se decide aqui. O `formato_som` vem de uma sondagem que
            // devolve `sem_eco: false` fixo — serve para o codificador saber o ritmo antes
            // de a captura existir, e mais nada. Decidir por ele fazia a app avisar
            // SEMPRE que o som ia com eco, mesmo quando não ia, e mandar desligar o som da
            // partilha sem razão nenhuma. Quem sabe a verdade é a captura, e é ela que
            // avisa agora.
            let envia_som = envia.clone();
            // `send` e não `try_send`: um bocado de som perdido é um buraco que fica, e os
            // buracos somam-se. O vídeo tem um teto próprio — ver `na_fila`.
            let avisa_som = aviso.clone();
            crate::som::arrancar(
                parar.clone(),
                origem,
                f,
                move |b| {
                    let _ = envia_som.send(Trabalho::Som(b.pcm, b.instante, b.duracao));
                },
                move |m| avisa_som(m),
            );
        }

        let vigia = parar.clone();
        std::thread::spawn(move || {
            // O item de captura constrói-se AQUI dentro: os tipos do capturador guardam
            // ponteiros COM que não atravessam threads; o Alvo é só números e atravessa.
            let flags = Flags {
                canal: envia,
                na_fila,
                lar,
                alt,
                parar,
                fps,
            };
            // Função e não closure: os dois ramos passam tipos diferentes (Monitor e
            // Window) e uma closure fixa-se no primeiro que vê.
            fn definicoes<T: TryInto<windows_capture::settings::GraphicsCaptureItemType>>(
                item: T,
                flags: Flags,
            ) -> Settings<Flags, T> {
                Settings::new(
                    item,
                    cursor(),
                    // A moldura amarela do Windows é o equivalente da barra do WebView2:
                    // se não sair, trocava-se um aviso por outro. Mas só onde ela SE DEIXA
                    // tirar — ver `moldura`.
                    moldura(),
                    SecondaryWindowSettings::Default,
                    // O ritmo pedido travado na ORIGEM: o Windows nem chega a capturar o
                    // frame que viria cedo demais. Travar aqui poupa a captura inteira —
                    // travar no nosso lado só pouparia a codificação, depois de a placa
                    // gráfica já ter feito o trabalho.
                    //
                    // MAS SÓ ONDE EXISTE. O `SetMinUpdateInterval` é API do Windows 11
                    // 24H2; onde ela não existe, a biblioteca não ignora o pedido — recusa
                    // a captura INTEIRA com `MinimumUpdateIntervalUnsupported`. A máquina
                    // onde isto se escreveu é 26200 e nunca deu sinal; a de quem tem
                    // Windows 10 ficaria sem partilha de ecrã nenhuma, e o sintoma seria
                    // "no meu funciona". Perguntar primeiro custa uma chamada e evita
                    // exactamente a classe de avaria que este projecto não consegue
                    // reproduzir em casa.
                    intervalo_minimo(flags.fps),
                    DirtyRegionSettings::Default,
                    ColorFormat::Bgra8,
                    flags,
                )
            }
            // `start_free_threaded` e não `start`, e a razão é uma avaria concreta: o
            // sinal de parar só era lido dentro do `on_frame_arrived`, e a captura do
            // Windows só entrega frames quando o ecrã MUDA. Numa janela parada ou
            // minimizada, parar de partilhar não parava nada — ficavam de pé a thread da
            // captura, a thread do codificador à espera no canal, e a sessão do codificador
            // da placa gráfica. Com o controlo na mão, o `stop()` manda um WM_QUIT e a
            // captura acaba mesmo, tenha havido frames ou não.
            //
            // Isto só passou a ser seguro hoje: enquanto o codificador vivia dentro da
            // `Sessao`, ela guardava ponteiros COM e precisava de um `unsafe impl Send` com
            // a promessa de nunca atravessar threads. Agora guarda um canal e números.
            let controlo = match alvo {
                super::Alvo::Ecra(i) => match Monitor::from_index(i) {
                    Ok(m) => Sessao::start_free_threaded(definicoes(m, flags)),
                    Err(e) => {
                        eprintln!("[ecrã] o ecrã desapareceu: {e:?}");
                        queixa(format!("esse ecrã já não existe: {e:?}"));
                        return;
                    }
                },
                super::Alvo::Janela(h) => Sessao::start_free_threaded(definicoes(
                    Window::from_raw_hwnd(h as *mut std::ffi::c_void),
                    flags,
                )),
            };
            // `BRUMA_FALHA_CAPTURA=1` finge uma captura que morre DEPOIS de o comando já
            // ter respondido `Ok`. É o único caminho de falha que não se consegue provocar
            // de fora, e era exactamente o que ficava invisível: sem consola em release, a
            // interface dizia "estás a partilhar" para sempre.
            let controlo = if crate::bandeiras::falha_captura() {
                Err(windows_capture::capture::GraphicsCaptureApiError::FailedToJoinThread)
            } else {
                controlo
            };
            let controlo = match controlo {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("[ecrã] a captura não arrancou: {e:?}");
                    queixa(format!(
                        "a captura de ecrã não arrancou neste Windows: {e:?}"
                    ));
                    return;
                }
            };
            // Fica-se aqui a vigiar o sinal, e não a dormir à espera de um frame.
            while !vigia.load(Ordering::Relaxed) && !controlo.is_finished() {
                std::thread::sleep(std::time::Duration::from_millis(120));
            }
            // AS DUAS SAÍDAS DESTE LAÇO NÃO SÃO A MESMA COISA (#39).
            //
            // `vigia` a `true` é uma paragem PEDIDA — a pessoa carregou no botão, ou uma
            // queixa anterior já a pôs. `is_finished()` sem o sinal é a captura a morrer
            // sozinha: a janela fechou, o monitor foi desligado, o driver foi reposto. Até
            // aqui as duas saídas eram tratadas como iguais, e a segunda passava sem uma
            // palavra: a interface continuava a dizer «estás a partilhar».
            //
            // O `pedido` lê-se ANTES do `stop()`, que é o que fecha o `Closed` do lado do
            // Windows: lê-lo depois confundia o sinal que a nossa própria queixa vai pôr.
            let pedido = vigia.load(Ordering::Relaxed);
            let sessao = controlo.callback();
            let resultado = controlo.stop();
            // O resumo sai SEMPRE por aqui, com a thread da captura já junta — tenha ela
            // acabado bem ou mal. Era dentro da tratadora, e uma captura que morre não
            // volta a correr a tratadora.
            eprintln!("{}", sessao.lock().resumo());
            if pedido {
                if let Err(e) = resultado {
                    eprintln!("[ecrã] ao parar a captura: {e:?}");
                }
            } else {
                use windows_capture::capture::{CaptureControlError, GraphicsCaptureApiError};
                let razao = match resultado {
                    // O texto do `on_closed` (ou de um `Err` da tratadora) vem por aqui.
                    Err(CaptureControlError::GraphicsCaptureApiError(
                        GraphicsCaptureApiError::FrameHandlerError(e),
                    )) => e.to_string(),
                    Err(outra) => format!("a captura de ecrã parou sozinha: {outra:?}"),
                    Ok(()) => "a janela ou o ecrã que estavas a partilhar desapareceu".into(),
                };
                eprintln!("[ecrã] a captura morreu sem ninguém pedir: {razao}");
                queixa(razao);
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
        _queixa: super::Queixa,
        _aviso: super::Aviso,
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
    queixa: Queixa,
    aviso: Aviso,
) -> Result<(u32, u32)> {
    if estado.a_partilhar() {
        return Err(anyhow!("já estás a partilhar"));
    }
    let parar = Arc::new(AtomicBool::new(false));
    let tamanho = win::arrancar(alvo, qualidade, parar.clone(), entrega, queixa, aviso)?;
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

#[cfg(all(test, windows))]
mod testes {
    /// A orla do conteúdo antigo (#167): uma janela que encolhe não pode deixar na margem
    /// o que lá estava antes.
    #[test]
    fn a_tela_nao_guarda_a_orla_do_frame_antigo() {
        let mut tela = Vec::new();
        let mut tela_de = (0, 0);
        // Primeiro frame: 4x4, tudo a 0xFF — enche a tela inteira.
        let cheio = vec![0xFFu8; 4 * 4 * 4];
        super::win::compor(&mut tela, &mut tela_de, (4, 4), &cheio, (4, 4));
        assert!(
            tela.iter().all(|b| *b == 0xFF),
            "o primeiro frame devia encher a tela"
        );
        // Segundo frame: a janela encolheu para 2x2, tudo a 0x11.
        let pequeno = vec![0x11u8; 2 * 2 * 4];
        super::win::compor(&mut tela, &mut tela_de, (4, 4), &pequeno, (2, 2));
        // O canto de 2x2 tem o frame novo...
        for y in 0..2 {
            for x in 0..2 {
                let i = (y * 4 + x) * 4;
                assert_eq!(
                    &tela[i..i + 4],
                    &[0x11; 4],
                    "pixel ({x},{y}) devia ser do frame novo"
                );
            }
        }
        // ...e TUDO o resto tem de estar a zero: era aqui que ficava a orla de 0xFF.
        let fora: usize = (0..4usize)
            .flat_map(|y| (0..4usize).map(move |x| (x, y)))
            .filter(|(x, y)| *x >= 2 || *y >= 2)
            .map(|(x, y)| (y * 4 + x) * 4)
            .filter(|i| tela[*i..*i + 4] != [0u8; 4])
            .count();
        assert_eq!(
            fora, 0,
            "{fora} pixels da margem ainda têm o conteúdo antigo: é a orla"
        );
    }
}
