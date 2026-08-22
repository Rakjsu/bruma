//! Spike 4 — capturar e codificar o ecrã em Rust, sem passar pelo `getDisplayMedia`.
//!
//! # Porque é que este spike existe
//!
//! A partilha de ecrã vive hoje dentro da WebView2, e isso trouxe duas coisas que não
//! queremos e que não se resolvem por configuração:
//!
//!  1. O WebView2 desenha por cima da app a barra "está a partilhar uma janela". Fui à
//!     documentação: o único gancho é o evento `ScreenCaptureStarting`, e ele só tem
//!     `Cancel` (bloqueia a partilha toda) e `Handled` (decide que handler corre
//!     primeiro). Nenhum esconde a barra, e não há flag. Faz sentido que não haja — a
//!     barra existe precisamente para nenhuma app capturar o ecrã sem o dono ver.
//!  2. O Spike 2 mediu o AV1 a correr por **software** (libaom) dentro do Chromium, com
//!     o custo de CPU que isso traz a 1080p60. A RTX que está nesta máquina tem
//!     codificador em hardware e ele não estava a ser usado.
//!
//! Capturar e codificar em Rust resolve as duas de uma vez: o WebView2 deixa de ter o
//! que anunciar, e escolhemos nós o codificador.
//!
//! # A decisão de desenho que este spike serve
//!
//! **Não vamos montar uma segunda pilha de WebRTC.** Já existe entre pares um transporte
//! autenticado e cifrado — o iroh. O caminho curto é: capturar em Rust, codificar em
//! Rust, mandar os NALs pelo iroh, e descodificar do outro lado com o `VideoDecoder` do
//! WebCodecs, que a WebView2 tem e acelera por hardware.
//!
//! Isso dispensa ICE e DTLS só para o vídeo, dispensa o TURN nesta parte (o iroh tem
//! relay próprio) e — não é detalhe pequeno — tira a partilha de ecrã da lista de coisas
//! que revelam o IP, porque o WebRTC fazia o seu próprio furo no NAT por fora de tudo.
//!
//! O preço é assumido: perdemos o controlo de congestão do WebRTC e passamos a ter de
//! fazer o nosso. O plano já queria esse controlo de qualquer maneira — é o capítulo do
//! orçamento de upload.
//!
//! # Os gates, por ordem de quanto matam o desenho
//!
//! * **G1 — o codificador é de hardware?** Se só houver software, trocávamos a barra do
//!   WebView2 por CPU queimada, que é pior do que o problema. É o primeiro a medir
//!   porque é o único que pode matar a ideia toda.
//! * **G2 — a captura chega a 60fps** com cursor, sem a moldura amarela do Windows, e
//!   diz-nos que regiões mudaram (é disso que vive a camada 1 do orçamento de upload:
//!   um ecrã parado deve custar quase nada).
//! * **G3 — o ritmo é estável.** Média boa com engasgos regulares dá vídeo aos solavancos.
//!   Por isso mede-se o p95 do intervalo entre frames, não só a média.
//!
//! Fica de fora deste spike, de propósito: o lado do WebCodecs a descodificar. Não se
//! testa a descodificação antes de existir alguma coisa medida para descodificar.

#[cfg(windows)]
mod medicoes {
    use anyhow::{anyhow, Result};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
    use windows_capture::encoder::{
        AudioSettingsBuilder, ContainerSettingsBuilder, VideoEncoder, VideoSettingsBuilder,
        VideoSettingsSubType,
    };
    use windows_capture::frame::Frame;
    use windows_capture::graphics_capture_api::InternalCaptureControl;
    use windows_capture::monitor::Monitor;
    use windows_capture::settings::{
        ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
        MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
    };

    /* ===================================================================== G1
    Que codificadores de H.264 existem nesta máquina, e quais são hardware.

    Isto pergunta-se ao Media Foundation em vez de se inferir do tempo de CPU: o
    `MFTEnumEx` devolve os codificadores registados e diz quais são acelerados. Um
    nome como "NVIDIA H.264 Encoder MFT" responde ao gate sem margem para dúvida,
    enquanto "medi pouco CPU, deve ser hardware" não responde a nada.
    ===================================================================== */
    pub fn codificadores() -> Result<usize> {
        use windows::Win32::Media::MediaFoundation::{
            IMFActivate, MFMediaType_Video, MFStartup, MFTEnumEx, MFT_FRIENDLY_NAME_Attribute,
            MFVideoFormat_H264, MFSTARTUP_FULL, MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG,
            MFT_ENUM_FLAG_ASYNCMFT, MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER,
            MFT_ENUM_FLAG_SYNCMFT, MFT_REGISTER_TYPE_INFO, MF_VERSION,
        };

        println!("G1 · codificadores de H.264 nesta máquina");

        unsafe {
            MFStartup(MF_VERSION, MFSTARTUP_FULL)?;

            let saida = MFT_REGISTER_TYPE_INFO {
                guidMajorType: MFMediaType_Video,
                guidSubtype: MFVideoFormat_H264,
            };

            let mut quantos = 0u32;
            let mut lista: *mut Option<IMFActivate> = std::ptr::null_mut();
            MFTEnumEx(
                MFT_CATEGORY_VIDEO_ENCODER,
                MFT_ENUM_FLAG(
                    MFT_ENUM_FLAG_HARDWARE.0
                        | MFT_ENUM_FLAG_SYNCMFT.0
                        | MFT_ENUM_FLAG_ASYNCMFT.0
                        | MFT_ENUM_FLAG_SORTANDFILTER.0,
                ),
                None,
                Some(&saida),
                &mut lista,
                &mut quantos,
            )?;

            let mut por_hardware = 0usize;
            for i in 0..quantos as usize {
                let Some(act) = (*lista.add(i)).clone() else {
                    continue;
                };
                let mut buf = [0u16; 256];
                let nome = match act.GetString(&MFT_FRIENDLY_NAME_Attribute, &mut buf, None) {
                    Ok(()) => String::from_utf16_lossy(&buf)
                        .trim_end_matches('\0')
                        .to_string(),
                    Err(_) => "(sem nome)".into(),
                };
                // A heurística do nome não decide nada — a ordem do SORTANDFILTER põe os
                // de hardware primeiro, mas quem confirma é o próprio nome do fabricante.
                let acelerado = ["nvidia", "intel", "amd", "qualcomm", "hardware"]
                    .iter()
                    .any(|m| nome.to_lowercase().contains(m));
                if acelerado {
                    por_hardware += 1;
                }
                println!(
                    "   {} {}",
                    if acelerado {
                        "[hardware]"
                    } else {
                        "[software]"
                    },
                    nome
                );
            }

            if quantos == 0 {
                println!("   nenhum — o Media Foundation não devolveu codificadores de H.264");
            }
            println!(
                "   → {} de {} parecem ser por hardware\n",
                por_hardware, quantos
            );
            Ok(por_hardware)
        }
    }

    /* ===================================================================== G2/G3
    Capturar o ecrã primário durante alguns segundos e medir o que interessa.
    ===================================================================== */

    /// O `CaptureControl::wait()` devolve `()` -- o handler morre dentro da thread da
    /// captura. Os numeros tem de sair por aqui.
    #[derive(Default)]
    pub struct Recolha {
        pub intervalos: Vec<f64>,
        pub com_regioes: usize,
        pub area_suja: Vec<f64>,
        pub bytes: u64,
        /// Milissegundos gastos por frame dentro do codificador (ou a ler o buffer).
        pub no_encoder: Vec<f64>,
    }

    pub struct Relatorio {
        pub codificou: bool,
        pub encoder_p50_ms: f64,
        pub frames: usize,
        pub segundos: f64,
        pub p50_ms: f64,
        pub p95_ms: f64,
        pub pior_ms: f64,
        /// Frames em que o Windows nos disse que regiões mudaram.
        pub com_regioes: usize,
        /// Fração média do ecrã que mudou, nos frames que trouxeram regiões.
        pub area_suja_media: f64,
        pub bytes: u64,
    }

    struct Sonda {
        encoder: Option<VideoEncoder>,
        stream: windows::Storage::Streams::InMemoryRandomAccessStream,
        inicio: Instant,
        ultimo: Instant,
        duracao: Duration,
        largura: u32,
        altura: u32,
        recolha: Arc<Mutex<Recolha>>,
    }

    /// (largura, altura, segundos, codificar?, onde deixar os numeros)
    type Flags = (u32, u32, u64, bool, Arc<Mutex<Recolha>>);

    impl GraphicsCaptureApiHandler for Sonda {
        type Flags = Flags;
        type Error = Box<dyn std::error::Error + Send + Sync>;

        fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
            let (largura, altura, segundos, codificar, recolha) = ctx.flags;
            let stream = windows::Storage::Streams::InMemoryRandomAccessStream::new()?;
            // Sem codificador, o unico custo por frame e a copia do buffer. E assim que
            // se sabe se os 2,5 fps da primeira medicao eram a captura ou o codificador.
            let encoder = if !codificar {
                None
            } else {
                Some(VideoEncoder::new_from_stream(
                    VideoSettingsBuilder::new(largura, altura)
                        .sub_type(VideoSettingsSubType::H264)
                        .frame_rate(60)
                        // 8 Mbps é o que se pediria a 1080p60 de jogo. Não é a proposta para
                        // produção — é um número fixo para a medição ser comparável.
                        .bitrate(8_000_000),
                    AudioSettingsBuilder::default().disabled(true),
                    ContainerSettingsBuilder::default(),
                    {
                        use windows::core::Interface;
                        stream.cast()?
                    },
                )?)
            };

            let agora = Instant::now();
            Ok(Self {
                encoder,
                stream,
                inicio: agora,
                ultimo: agora,
                duracao: Duration::from_secs(segundos),
                largura,
                altura,
                recolha,
            })
        }

        fn on_frame_arrived(
            &mut self,
            frame: &mut Frame,
            controlo: InternalCaptureControl,
        ) -> Result<(), Self::Error> {
            let agora = Instant::now();
            let intervalo = agora.duration_since(self.ultimo).as_secs_f64() * 1000.0;
            self.ultimo = agora;
            self.recolha.lock().unwrap().intervalos.push(intervalo);

            // Que parte do ecrã mudou. É isto que decide se um ecrã parado custa quase
            // nada — sem regiões sujas, codifica-se o ecrã inteiro 60 vezes por segundo
            // para mostrar um cursor a piscar.
            if let Ok(regioes) = frame.dirty_regions() {
                if !regioes.is_empty() {
                    let total: f64 = regioes
                        .iter()
                        .map(|r| (r.width.max(0) as f64) * (r.height.max(0) as f64))
                        .sum();
                    let ecra = (self.largura as f64) * (self.altura as f64);
                    let mut r = self.recolha.lock().unwrap();
                    r.com_regioes += 1;
                    if ecra > 0.0 {
                        r.area_suja.push((total / ecra).min(1.0));
                    }
                }
            }

            // Mede-se quanto tempo o codificador rouba a cada frame: se `send_frame`
            // bloquear, e ele que manda no ritmo, nao a captura.
            if let Some(enc) = self.encoder.as_mut() {
                let antes = Instant::now();
                enc.send_frame(frame)?;
                let gasto = antes.elapsed().as_secs_f64() * 1000.0;
                self.recolha.lock().unwrap().no_encoder.push(gasto);
            } else {
                // Sem codificador ainda se toca no buffer, senao media-se uma captura
                // que ninguem le -- e o Windows pode nem a entregar.
                let antes = Instant::now();
                let _ = frame.buffer()?;
                let gasto = antes.elapsed().as_secs_f64() * 1000.0;
                self.recolha.lock().unwrap().no_encoder.push(gasto);
            }

            if agora.duration_since(self.inicio) >= self.duracao {
                if let Some(enc) = self.encoder.take() {
                    enc.finish()?;
                    // O tamanho le-se depois do finish(): antes disso faltam os
                    // cabecalhos que o muxer so escreve no fim.
                    self.recolha.lock().unwrap().bytes = self.stream.Size().unwrap_or(0);
                }
                controlo.stop();
            }
            Ok(())
        }

        fn on_closed(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    fn percentil(ordenados: &[f64], p: f64) -> f64 {
        if ordenados.is_empty() {
            return 0.0;
        }
        let i = ((ordenados.len() - 1) as f64 * p).round() as usize;
        ordenados[i]
    }

    pub fn capturar(segundos: u64, codificar: bool) -> Result<Relatorio> {
        let monitor = Monitor::primary().map_err(|e| anyhow!("sem monitor primário: {e:?}"))?;
        let largura = monitor.width().map_err(|e| anyhow!("{e:?}"))?;
        let altura = monitor.height().map_err(|e| anyhow!("{e:?}"))?;
        let hz = monitor.refresh_rate().unwrap_or(0);

        let recolha = Arc::new(Mutex::new(Recolha::default()));

        println!(
            "   {largura}x{altura} a {hz} Hz, durante {segundos}s, {}",
            if codificar {
                "a codificar"
            } else {
                "sem codificar"
            }
        );

        let definicoes = Settings::new(
            monitor,
            // O cursor faz parte do que se quer partilhar.
            CursorCaptureSettings::WithCursor,
            // A moldura amarela do Windows é o equivalente da barra do WebView2. Se não
            // sair, trocamos um aviso por outro e não ganhámos nada.
            DrawBorderSettings::WithoutBorder,
            SecondaryWindowSettings::Default,
            MinimumUpdateIntervalSettings::Default,
            // ReportOnly: queremos saber o que mudou, não que o Windows nos desenhe as
            // regiões separadas.
            DirtyRegionSettings::ReportOnly,
            ColorFormat::Bgra8,
            (largura, altura, segundos, codificar, recolha.clone()),
        );

        // O `start` toma conta desta thread e só devolve no fim da captura; o relatório
        // sai por um canal porque o handler é consumido lá dentro.
        let comeco = Instant::now();
        let controlo = Sonda::start_free_threaded(definicoes)
            .map_err(|e| anyhow!("a captura não arrancou: {e:?}"))?;
        controlo
            .wait()
            .map_err(|e| anyhow!("a captura falhou a meio: {e:?}"))?;
        let decorrido = comeco.elapsed().as_secs_f64();

        let r = recolha.lock().unwrap();
        // O primeiro intervalo mede o arranque da captura, nao o ritmo dela -- fora.
        let mut ordenados: Vec<f64> = r.intervalos.iter().skip(1).copied().collect();
        ordenados.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let media_suja = if r.area_suja.is_empty() {
            0.0
        } else {
            r.area_suja.iter().sum::<f64>() / r.area_suja.len() as f64
        };

        let mut enc = r.no_encoder.clone();
        enc.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        Ok(Relatorio {
            codificou: codificar,
            encoder_p50_ms: percentil(&enc, 0.50),
            frames: r.intervalos.len(),
            segundos: decorrido,
            p50_ms: percentil(&ordenados, 0.50),
            p95_ms: percentil(&ordenados, 0.95),
            pior_ms: ordenados.last().copied().unwrap_or(0.0),
            com_regioes: r.com_regioes,
            area_suja_media: media_suja,
            bytes: r.bytes,
        })
    }
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    let segundos: u64 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(8);

    println!("-- Spike 4 . captura nativa ------------------------------\n");

    let hardware = medicoes::codificadores()?;

    // Duas passagens sobre a MESMA cena, so a mudar se ha codificador. Uma medicao
    // sozinha nao distingue "a captura e lenta" de "o codificador e que trava tudo",
    // e as duas conclusoes levam a sitios opostos.
    println!("G2/G3 . captura do ecra primario");
    let sem = medicoes::capturar(segundos, false)?;
    mostrar(&sem);
    let com = medicoes::capturar(segundos, true)?;
    mostrar(&com);

    let fps_sem = ritmo(&sem);
    let fps_com = ritmo(&com);

    println!("\n-- gates -------------------------------------------------");
    veredicto("G1 . codificador por hardware existe", hardware > 0);
    veredicto("G2 . captura chega aos 50 fps", fps_sem > 50.0);
    veredicto("G2 . regioes sujas disponiveis", sem.com_regioes > 0);
    veredicto(
        "G3 . ritmo estavel (p95 < 40 ms)",
        sem.p95_ms > 0.0 && sem.p95_ms < 40.0,
    );
    veredicto(
        "G3 . codificar nao estrangula (perde < 20%)",
        fps_sem > 0.0 && fps_com >= fps_sem * 0.8,
    );

    if fps_sem > 0.0 {
        println!(
            "\n   codificar custa {:.0}% do ritmo ({:.1} -> {:.1} fps), {:.1} ms por frame",
            (1.0 - fps_com / fps_sem) * 100.0,
            fps_sem,
            fps_com,
            com.encoder_p50_ms
        );
    }

    Ok(())
}

#[cfg(windows)]
fn ritmo(r: &medicoes::Relatorio) -> f64 {
    if r.segundos > 0.0 {
        r.frames as f64 / r.segundos
    } else {
        0.0
    }
}

#[cfg(windows)]
fn mostrar(r: &medicoes::Relatorio) {
    let fps = ritmo(r);
    let mbps = if r.segundos > 0.0 {
        (r.bytes as f64 * 8.0) / r.segundos / 1_000_000.0
    } else {
        0.0
    };
    println!(
        "   {:<14} {:>5} frames em {:.1}s = {:>5.1} fps | p50/p95 {:>5.1}/{:>5.1} ms | \
         pior {:>5.1} ms | sujo {:>4.1}% | {:>5.1} ms no {} | {:.1} Mbps",
        if r.codificou {
            "a codificar:"
        } else {
            "so a captar:"
        },
        r.frames,
        r.segundos,
        fps,
        r.p50_ms,
        r.p95_ms,
        r.pior_ms,
        r.area_suja_media * 100.0,
        r.encoder_p50_ms,
        if r.codificou { "encoder" } else { "buffer " },
        mbps
    );
}

#[cfg(windows)]
fn veredicto(nome: &str, passou: bool) {
    println!("   {} {}", if passou { "PASSA " } else { "FALHA " }, nome);
}

#[cfg(not(windows))]
fn main() {
    println!("Spike 4 só corre no Windows: usa Windows.Graphics.Capture e Media Foundation.");
}
