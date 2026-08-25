//! O som que sai das colunas, captado para seguir com a partilha de ecrã.
//!
//! # Porquê o `loopback` do WASAPI e não o navegador
//!
//! O caminho fácil seria `getDisplayMedia({audio: true})`, mas foi por causa dele que se
//! escreveu a captura nativa: traz a barra "está a partilhar" do WebView2, que não se
//! desliga. E o "Stereo Mix" — a outra saída fácil — vem desligado na maioria das placas e
//! obrigaria a pessoa a ir ao painel de som do Windows. Era exactamente a configuração
//! manual que este projecto passa a vida a apagar.
//!
//! O `loopback` do WASAPI não precisa de nada disso: pede-se ao dispositivo de SAÍDA que
//! entregue o que está a tocar. Sem drivers, sem definições, sem permissões.
//!
//! # O silêncio tem de ser fabricado
//!
//! Quando nada toca, o WASAPI em `loopback` não entrega pacote nenhum — não entrega
//! silêncio, entrega NADA. Se se deixasse assim, o contentor ficava com buracos e o som do
//! outro lado dessincronizava do vídeo assim que alguém fizesse uma pausa na música. Por
//! isso, quando o relógio avança e não veio nada, fabrica-se o silêncio que falta.

use anyhow::Result;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// O formato que o dispositivo nos deu. O WASAPI entrega no formato de mistura do
/// dispositivo e não naquele que nós pedimos — daí ser preciso ler o que ele diz.
#[derive(Clone, Copy, Debug)]
pub struct Formato {
    pub ritmo: u32,
    pub canais: u16,
    /// Bits por amostra por canal, já como o entregamos: 16. Fica escrito porque é a
    /// promessa que o `para_i16` cumpre e que o codificador lê.
    #[allow(dead_code)]
    pub bits: u16,
    /// Se a captura consegue excluir o som da própria app.
    ///
    /// Quando é `false`, o que sai das colunas por ordem do Bruma — a voz das outras
    /// pessoas na chamada — volta a entrar na partilha e é reenviado. Ver `abrir_cliente`.
    pub sem_eco: bool,
}

/// Um bocado de som, em PCM 16 bits intercalado, com o instante em que começou.
pub struct Bocado {
    pub pcm: Vec<u8>,
    /// Unidades de 100 ns desde o início da captura — a mesma unidade do vídeo.
    pub instante: i64,
    pub duracao: i64,
}

#[cfg(windows)]
mod win {
    use super::{Bocado, Formato, Result};
    use anyhow::anyhow;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
    use windows::Win32::Media::Audio::{
        eConsole, eRender, ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
        IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl,
        IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator,
        AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
        AUDCLNT_STREAMFLAGS_LOOPBACK, AUDIOCLIENT_ACTIVATION_PARAMS,
        AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
        PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
        PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
        WAVEFORMATEX, WAVEFORMATEXTENSIBLE, WAVE_FORMAT_PCM,
    };
    use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForSingleObject};
    // Estas três vivem noutros módulos do mesmo SDK, e é por isso que estão à parte.
    use windows::Win32::Media::KernelStreaming::WAVE_FORMAT_EXTENSIBLE;
    use windows::Win32::Media::Multimedia::{
        KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED,
    };

    /// Uma amostra do dispositivo, seja qual for o formato dele, como um número entre
    /// -1 e 1. Devolve `None` para formatos que não se sabem ler — e nesse caso prefere-se
    /// silêncio a adivinhar, porque adivinhar aqui soa a ruído branco a todo o volume.
    fn amostra(bytes: &[u8], i: usize, forma: Forma) -> Option<f32> {
        match forma {
            Forma::Float32 => {
                let c = bytes.get(i * 4..i * 4 + 4)?;
                Some(f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            }
            Forma::Int16 => {
                let c = bytes.get(i * 2..i * 2 + 2)?;
                Some(i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
            }
            Forma::Int32 => {
                let c = bytes.get(i * 4..i * 4 + 4)?;
                Some(i32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f32 / 2_147_483_648.0)
            }
            Forma::Int24 => {
                let c = bytes.get(i * 3..i * 3 + 3)?;
                // 24 bits com sinal, em três bytes little-endian: sobe-se para 32 bits e
                // desce-se outra vez, que é a forma barata de estender o sinal.
                let v = ((c[0] as i32) << 8) | ((c[1] as i32) << 16) | ((c[2] as i32) << 24);
                Some(v as f32 / 2_147_483_648.0)
            }
            Forma::Desconhecida => None,
        }
    }

    /// Quantos bytes ocupa uma amostra, por forma. Substitui o `bits` do dispositivo, que
    /// deixou de existir aqui: com loopback de processo é o nosso pedido que manda, e a
    /// forma é a única fonte de verdade nos dois caminhos.
    fn bytes_por_amostra(forma: Forma) -> usize {
        match forma {
            Forma::Int16 => 2,
            Forma::Int24 => 3,
            Forma::Float32 | Forma::Int32 => 4,
            Forma::Desconhecida => 0,
        }
    }

    /// Converte o que o dispositivo entrega para PCM de 16 bits **em estéreo**, que é o que
    /// se declara ao codificador de AAC.
    ///
    /// # Porque é que isto tem de misturar
    ///
    /// O contentor é informado de DOIS canais. Se o dispositivo for 5.1 e se lhe passassem
    /// seis, o Media Foundation leria os mesmos bytes como três vezes mais quadros de
    /// estéreo — o som sairia ao triplo da velocidade e agudo. Não estoirava; soava mal, e
    /// só em casa de quem tem 5.1. É o tipo de avaria que nunca se reproduz onde se
    /// desenvolve.
    ///
    /// A mistura segue a receita habitual (ITU-R BS.775): a frente vai direta, o centro
    /// entra nos dois lados a -3 dB — é onde vive a voz, e deixá-lo cair era perder
    /// exactamente o que interessa — as traseiras entram também a -3 dB, e o LFE fica de
    /// fora, porque num altifalante de portátil só faria estalar.
    fn para_estereo(bytes: &[u8], forma: Forma, canais: u16) -> Vec<u8> {
        let n = canais.max(1) as usize;
        let largura = bytes_por_amostra(forma);
        if largura == 0 {
            return Vec::new();
        }
        let quadros = bytes.len() / (largura * n);
        let mut v = Vec::with_capacity(quadros * 4);
        // -3 dB, que é o factor com que o centro e as traseiras entram na mistura. É
        // literalmente 1/raiz(2), e o Rust já o tem escrito com todos os dígitos.
        const MEIA: f32 = std::f32::consts::FRAC_1_SQRT_2;
        for q in 0..quadros {
            let base = q * n;
            let le = |c: usize| amostra(bytes, base + c, forma).unwrap_or(0.0);
            let (mut e, mut d) = if n == 1 {
                let m = le(0);
                (m, m)
            } else {
                (le(0), le(1))
            };
            if n >= 3 {
                let centro = le(2) * MEIA;
                e += centro;
                d += centro;
            }
            // 5.1 = FL FR FC LFE BL BR — salta-se o 3 (LFE) de propósito.
            if n >= 6 {
                e += le(4) * MEIA;
                d += le(5) * MEIA;
            }
            for canal in [e, d] {
                // O clamp não é zelo a mais: somar canais passa de 1.0 com facilidade, e
                // sem isto dava a volta ao número — estalo em vez de saturar.
                let i = (canal.clamp(-1.0, 1.0) * 32767.0) as i16;
                v.extend_from_slice(&i.to_le_bytes());
            }
        }
        v
    }

    /// Como o dispositivo escreve cada amostra.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Forma {
        Float32,
        Int32,
        Int24,
        Int16,
        Desconhecida,
    }

    /// Lê a forma do `WAVEFORMATEX`, descendo ao `WAVEFORMATEXTENSIBLE` quando é preciso.
    ///
    /// O teste antigo era `wFormatTag != PCM && bits == 32` — e isso trata um dispositivo
    /// de 32 bits INTEIROS como se fosse float, o que transforma música em ruído a todo o
    /// volume. O `wFormatTag` de um formato moderno é quase sempre `EXTENSIBLE`, e quem
    /// sabe a verdade é o `SubFormat` que vem a seguir.
    unsafe fn forma_de(mix: *const WAVEFORMATEX) -> Forma {
        let bits = (*mix).wBitsPerSample;
        let etiqueta = (*mix).wFormatTag as u32;
        let float = if etiqueta == WAVE_FORMAT_EXTENSIBLE {
            let ext = mix as *const WAVEFORMATEXTENSIBLE;
            // `read_unaligned` e não `(*ext).SubFormat`: a estrutura é *packed*, e tirar uma
            // referência a um campo dela é comportamento indefinido mesmo que nunca se
            // desreferencie. O compilador recusa-o, e faz bem.
            std::ptr::read_unaligned(std::ptr::addr_of!((*ext).SubFormat))
                == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
        } else {
            etiqueta == WAVE_FORMAT_IEEE_FLOAT
        };
        match (float, bits) {
            (true, 32) => Forma::Float32,
            (false, 32) => Forma::Int32,
            (false, 24) => Forma::Int24,
            (false, 16) => Forma::Int16,
            _ => Forma::Desconhecida,
        }
    }

    /// Um cliente de áudio aberto e pronto, com tudo o que o ciclo de captura precisa.
    #[cfg(windows)]
    struct Aberto {
        cliente: IAudioClient,
        /// O que se ANUNCIA ao contentor: sempre estéreo de 16 bits.
        formato: Formato,
        /// Como ler cada amostra do que o dispositivo entrega.
        forma: Forma,
        /// Quantos canais o dispositivo entrega — pode não ser 2, e é isso que o
        /// `para_estereo` mistura.
        canais_crus: u16,
        /// Quando existe, a captura espera neste evento em vez de dormir.
        evento: Option<HANDLE>,
    }

    /// O processo que toca REALMENTE o nosso som.
    ///
    /// # A descoberta que custou o teste
    ///
    /// A ideia óbvia era excluir a nossa própria árvore de processos. Não funciona, e
    /// mediu-se: com `INCLUDE_TARGET_PROCESS_TREE` sobre o nosso PID, a captura devolve
    /// **silêncio absoluto** enquanto a app toca um tom bem alto. Ou seja, o Windows não
    /// liga o `msedgewebview2.exe` ao `bruma.exe` para efeitos de áudio — apesar de ele
    /// ser filho directo, coisa que foi confirmada à parte (um filho e seis netos).
    ///
    /// Quem toca é o WebView2. É ele o alvo. Procura-se o filho directo com esse nome; a
    /// árvore DELE cobre os outros seis, que são os que fazem o trabalho.
    ///
    /// Sem isto, a "correcção" do eco não corrigia nada e teria passado por boa: o
    /// `semEco` dizia `true`, a activação corria sem erro, e a voz continuava a voltar.
    #[cfg(windows)]
    fn pid_que_toca_por_nos() -> u32 {
        use windows::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
            TH32CS_SNAPPROCESS,
        };
        let nosso = std::process::id();
        unsafe {
            let Ok(foto) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
                return nosso;
            };
            let mut e = PROCESSENTRY32W {
                dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };
            let mut achado = nosso;
            if Process32FirstW(foto, &mut e).is_ok() {
                loop {
                    if e.th32ParentProcessID == nosso {
                        let nome = String::from_utf16_lossy(&e.szExeFile);
                        if nome.to_lowercase().starts_with("msedgewebview2") {
                            achado = e.th32ProcessID;
                            break;
                        }
                    }
                    if Process32NextW(foto, &mut e).is_err() {
                        break;
                    }
                }
            }
            let _ = windows::Win32::Foundation::CloseHandle(foto);
            if achado == nosso {
                eprintln!("[som] não encontrei o processo do WebView2; a exclusão do eco não vai apanhar tudo");
            }
            achado
        }
    }

    /// O que se manda ao Windows para lhe pedir só o som dos OUTROS processos.
    ///
    /// # A avaria que isto corrige
    ///
    /// O loopback de endpoint entrega a **mistura final do dispositivo** — tudo o que
    /// qualquer processo lá pôs. Inclusive nós: a voz das outras pessoas na chamada sai
    /// pelas colunas por ordem do Bruma, e voltava a entrar na partilha de ecrã. Quem
    /// estava do outro lado **ouvia-se a si próprio**, com o atraso do caminho todo.
    ///
    /// O `echoCancellation` do microfone não ajuda: só actua no caminho do microfone, e
    /// este eco é captado digitalmente a jusante do misturador do Windows.
    ///
    /// A cura é pedir loopback **de processo** com `EXCLUDE_TARGET_PROCESS_TREE` sobre o
    /// nosso PID: o Windows entrega o jogo, a música e o vídeo, e cala o que somos nós a
    /// tocar. A árvore inclui os processos do WebView2, que são quem toca a voz.
    #[cfg(windows)]
    unsafe fn parametros_sem_eco(nosso_pid: u32) -> AUDIOCLIENT_ACTIVATION_PARAMS {
        let mut p = AUDIOCLIENT_ACTIVATION_PARAMS {
            ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            ..Default::default()
        };
        p.Anonymous.ProcessLoopbackParams = AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
            TargetProcessId: nosso_pid,
            // `BRUMA_SO_NOS=1` inverte: em vez de tudo MENOS nós, passa a ser só nós.
            // Existe para o diagnóstico poder distinguir "os parâmetros não chegam" de
            // "chegam e a exclusão não faz o que eu penso" — com INCLUDE, se o nosso tom
            // aparecer, os parâmetros chegam de certeza.
            ProcessLoopbackMode: if std::env::var("BRUMA_SO_NOS").is_ok() {
                PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE
            } else {
                PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE
            },
        };
        p
    }

    /// O tratador que o `ActivateAudioInterfaceAsync` exige. Só serve para acordar quem
    /// espera — o resultado vai-se buscar a seguir, ao `GetActivateResult`.
    #[cfg(windows)]
    #[windows::core::implement(IActivateAudioInterfaceCompletionHandler)]
    struct Acordar(HANDLE);

    #[cfg(windows)]
    impl IActivateAudioInterfaceCompletionHandler_Impl for Acordar_Impl {
        fn ActivateCompleted(
            &self,
            _op: windows_core::Ref<'_, IActivateAudioInterfaceAsyncOperation>,
        ) -> windows_core::Result<()> {
            unsafe { SetEvent(self.0)? };
            Ok(())
        }
    }

    /// O ritmo que se pede ao loopback de processo. Ao contrário do de endpoint, aqui não
    /// se pergunta nada ao dispositivo: é o pedido que manda e o Windows converte. Pede-se
    /// o que o codificador de AAC quer, e o `para_estereo` fica a passar bytes sem os tocar.
    #[cfg(windows)]
    const RITMO_PEDIDO: u32 = 48_000;

    /// Abre o cliente, preferindo o caminho SEM ECO e caindo no antigo quando não houver.
    ///
    /// O loopback de processo é do Windows 10 2004 em diante. Onde não existir, o de
    /// endpoint funciona — e traz o eco atrás. Quem chama tem de o **dizer**, não esconder.
    #[cfg(windows)]
    unsafe fn abrir_cliente() -> Result<Aberto> {
        // `BRUMA_ECO_ANTIGO=1` força o caminho de endpoint. É o CONTROLO do teste: se
        // nem ele ouvir o que a app toca, o problema está no teste e não na exclusão.
        if std::env::var("BRUMA_ECO_ANTIGO").is_ok() {
            eprintln!("[som] a usar o loopback de endpoint, a pedido");
        } else {
            match tentar_sem_eco() {
                Ok(a) => return Ok(a),
                Err(e) => eprintln!(
                    "[som] este Windows não sabe captar só o som dos outros processos ({e}); \
                 a partilha vai levar a voz da chamada de volta"
                ),
            }
        }
        let enumerador: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let dispositivo = enumerador.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let cliente: IAudioClient = dispositivo.Activate(CLSCTX_ALL, None)?;
        let mix = cliente.GetMixFormat()?;
        let (ritmo, canais_crus) = ((*mix).nSamplesPerSec, (*mix).nChannels);
        let forma = forma_de(mix);
        let r = cliente.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK,
            2_000_000,
            0,
            mix,
            None,
        );
        CoTaskMemFree(Some(mix as *const _));
        r?;
        Ok(Aberto {
            cliente,
            formato: Formato {
                ritmo,
                canais: 2,
                bits: 16,
                sem_eco: false,
            },
            forma,
            canais_crus,
            evento: None,
        })
    }

    /// A tentativa boa. Falha limpa em Windows anterior ao 2004.
    #[cfg(windows)]
    unsafe fn tentar_sem_eco() -> Result<Aberto> {
        use windows::Win32::System::Com::StructuredStorage::{
            PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
        };
        use windows::Win32::System::Com::BLOB;
        use windows::Win32::System::Variant::VT_BLOB;
        use windows_core::Interface;

        let pronto = CreateEventW(None, false, false, None)?;
        let mut params = parametros_sem_eco(pid_que_toca_por_nos());

        // Um PROPVARIANT montado à mão: `VT_BLOB` a apontar para os parâmetros. Não há
        // construtor para isto no crate, e o `params` tem de continuar vivo até ao fim da
        // chamada — por isso é uma variável local e não uma temporária.
        // `ManuallyDrop` NÃO é zelo: o `PROPVARIANT` do crate tem um `Drop` que chama o
        // `PropVariantClear`, e para um `VT_BLOB` isso tenta libertar o `pBlobData` como
        // memória COM. O nosso aponta para a PILHA — o resultado era o processo a morrer
        // sem pânico e sem uma linha de erro, ao SAIR desta função.
        //
        // Custou a encontrar porque o `impl Drop` não está no módulo gerado, onde se
        // procura, mas em `src/extensions/`. Procurar no sítio óbvio devolveu vazio e
        // desviou-me para suspeitas erradas durante uns bons minutos.
        let pv = std::mem::ManuallyDrop::new(PROPVARIANT {
            Anonymous: PROPVARIANT_0 {
                Anonymous: std::mem::ManuallyDrop::new(PROPVARIANT_0_0 {
                    vt: VT_BLOB,
                    wReserved1: 0,
                    wReserved2: 0,
                    wReserved3: 0,
                    Anonymous: PROPVARIANT_0_0_0 {
                        blob: BLOB {
                            cbSize: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
                            pBlobData: &mut params as *mut _ as *mut u8,
                        },
                    },
                }),
            },
        });

        let tratador: IActivateAudioInterfaceCompletionHandler = Acordar(pronto).into();
        let operacao = ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&*pv),
            &tratador,
        )?;

        // Cinco segundos é folga a mais para uma activação local; existe só para nunca
        // ficar pendurado — a lição do `getUserMedia` sem tratador de permissões.
        let esperou = WaitForSingleObject(pronto, 5_000);
        let _ = CloseHandle(pronto);
        if esperou != WAIT_OBJECT_0 {
            return Err(anyhow!("a activação não respondeu"));
        }

        let mut hr = windows_core::HRESULT(0);
        let mut interface: Option<windows_core::IUnknown> = None;
        operacao.GetActivateResult(&mut hr, &mut interface)?;
        hr.ok()?;
        let cliente: IAudioClient = interface.ok_or_else(|| anyhow!("sem cliente"))?.cast()?;

        let wfx = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: 2,
            nSamplesPerSec: RITMO_PEDIDO,
            nAvgBytesPerSec: RITMO_PEDIDO * 4,
            nBlockAlign: 4,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        // O loopback de processo EXIGE o modo por evento; sem ele o `Initialize` recusa.
        let evento = CreateEventW(None, false, false, None)?;
        cliente.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
            2_000_000,
            0,
            &wfx,
            None,
        )?;
        cliente.SetEventHandle(evento)?;

        Ok(Aberto {
            cliente,
            formato: Formato {
                ritmo: RITMO_PEDIDO,
                canais: 2,
                bits: 16,
                sem_eco: true,
            },
            forma: Forma::Int16,
            canais_crus: 2,
            evento: Some(evento),
        })
    }

    /// O formato do dispositivo de saída, sem começar a captar.
    ///
    /// Existe separado porque o codificador precisa de saber o ritmo e os canais para
    /// nascer, e só depois de ele nascer é que o som pode começar — de outra forma os dois
    /// relógios arrancavam em instantes diferentes e o som ficava adiantado.
    pub fn formato() -> Result<Formato> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let enumerador: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let dispositivo = enumerador.GetDefaultAudioEndpoint(eRender, eConsole)?;
            let cliente: IAudioClient = dispositivo.Activate(CLSCTX_ALL, None)?;
            let mix = cliente.GetMixFormat()?;
            let f = Formato {
                ritmo: (*mix).nSamplesPerSec,
                canais: 2,
                bits: 16,
                // Este caminho só serve para o codificador saber o ritmo antes de a
                // captura começar; quem decide mesmo é o `abrir_cliente`. Fica `false`
                // porque ainda não foi ganho — e por isso NINGUÉM deve tirar conclusões
                // daqui. Quem quer saber se há eco tem de esperar pelo `anunciar` da
                // captura, que traz o formato a sério.
                sem_eco: false,
            };
            CoTaskMemFree(Some(mix as *const _));
            Ok(f)
        }
    }

    /// Capta o que está a tocar e chama `entrega` com cada bocado, até `parar`.
    ///
    /// Corre na thread que a chama — quem quiser noutra, que a lance.
    pub fn captar(
        parar: Arc<AtomicBool>,
        origem: std::time::Instant,
        anunciar: impl FnOnce(Formato),
        mut entrega: impl FnMut(Bocado),
        // Onde a faixa de som já vai, para quem tiver de continuar daqui. Se a captura
        // morrer a meio, o silêncio de recurso TEM de continuar deste ponto: recomeçar em
        // zero mandava amostras para trás no tempo e partia a linha de quem recebe.
        cursor: &std::sync::atomic::AtomicU64,
    ) -> Result<()> {
        unsafe {
            // O `loopback` é COM puro; sem isto o CoCreateInstance falha na thread nova.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

            // Prefere o loopback DE PROCESSO, que exclui a nossa própria voz. Ver
            // `abrir_cliente` — é a diferença entre partilhar o som do jogo e devolver a
            // voz de toda a gente com atraso.
            let aberto = abrir_cliente()?;
            let Aberto {
                cliente,
                formato,
                forma,
                canais_crus,
                evento,
            } = aberto;
            let ritmo = formato.ritmo;
            let canais = canais_crus;
            if forma == Forma::Desconhecida {
                eprintln!(
                    "[som] formato que não se sabe ler; a partilha vai muda em vez de ir \
                     com ruído"
                );
            }

            let captura: IAudioCaptureClient = cliente.GetService()?;
            cliente.Start()?;

            anunciar(formato);

            // JÁ EM ESTÉREO: é o que se declara ao codificador, e tem de ser o que se lhe
            // entrega. Ver `para_estereo`.
            let bytes_por_frame = 4;
            let inicio = std::time::Instant::now();
            // O som não começa no instante zero da partilha: o codificador nasceu primeiro
            // e o dispositivo levou o seu tempo a abrir.
            //
            // Isto era somado ao `instante` de cada bocado — e não servia de nada. O único
            // sítio do fluxo que carrega tempo é o `tfdt`, e o `mse.rs` fabrica-o a partir
            // da SOMA DAS DURAÇÕES de cada faixa (mse.rs:297): o instante que o Media
            // Foundation escreve é descartado. Toda a faixa começa obrigatoriamente em
            // zero, e o comentário que aqui estava — «é o que mantém o som colado à
            // imagem» — descrevia uma coisa que não acontecia.
            //
            // Põe-se o atraso na MATÉRIA em vez de no carimbo: um bocado de silêncio com a
            // duração do atraso, e daí para a frente tudo conta a partir dele. Assim a
            // soma das durações carrega o desvio sozinha, sem depender de ninguém o ler.
            let atraso = (origem.elapsed().as_nanos() / 100) as i64;
            let frames_do_atraso = (atraso.max(0) as u64) * ritmo as u64 / 10_000_000;
            if frames_do_atraso > 0 {
                entrega(Bocado {
                    pcm: vec![0u8; frames_do_atraso as usize * bytes_por_frame],
                    instante: 0,
                    duracao: (frames_do_atraso * 10_000_000 / ritmo as u64) as i64,
                });
            }
            // Onde vai o som já entregue, em frames. É este contador — e não o relógio da
            // parede — que decide quanto silêncio falta: assim o som nunca fica com mais
            // nem menos amostras do que o tempo que passou.
            let mut entregues: u64 = frames_do_atraso;

            // O dispositivo de som não morre a pedido, e um ramo que nunca corre é um
            // ramo por verificar — sobretudo este, que durante versões transformou uma
            // avaria em silêncio perfeito.
            let morre_aos = std::env::var("BRUMA_SOM_MORRE")
                .ok()
                .and_then(|v| v.parse::<u64>().ok());

            while !parar.load(Ordering::Relaxed) {
                if let Some(s) = morre_aos {
                    if inicio.elapsed().as_secs() >= s {
                        anyhow::bail!("dispositivo de som invalidado (BRUMA_SOM_MORRE)");
                    }
                }
                // `unwrap_or(0)` aqui era mortal e calado. Zero significa "ninguém está a
                // tocar nada", e é exactamente o que o WASAPI devolve num Err quando o
                // dispositivo é invalidado -- auscultadores ligados, monitor a adormecer,
                // o Windows a mudar o dispositivo por omissão. O laço entrava no ramo do
                // silêncio e fabricava zeros ao ritmo certo, com carimbos certos, para
                // sempre: quem assiste recebia uma faixa de som perfeita e muda, e quem
                // partilha continuava a ouvir tudo nas suas colunas sem razão para
                // desconfiar.
                let mut pacote = captura.GetNextPacketSize()?;
                if pacote == 0 {
                    // Com loopback de processo há um evento a avisar; sem ele, dorme-se.
                    // O prazo é curto de propósito: quando nada toca o Windows não acorda
                    // ninguém, e é o tempo a passar que decide quanto silêncio falta.
                    if let Some(e) = evento {
                        WaitForSingleObject(e, 20);
                    }
                    // Nada a tocar. Vê-se quanto silêncio falta para o som acompanhar o
                    // relógio, e fabrica-se.
                    let decorrido = inicio.elapsed().as_secs_f64();
                    let devidos = (decorrido * ritmo as f64) as u64;
                    if devidos > entregues + ritmo as u64 / 50 {
                        let faltam = (devidos - entregues) as usize;
                        let instante = (entregues * 10_000_000 / ritmo as u64) as i64;
                        entrega(Bocado {
                            pcm: vec![0u8; faltam * bytes_por_frame],
                            instante,
                            duracao: (faltam as u64 * 10_000_000 / ritmo as u64) as i64,
                        });
                        cursor.store(devidos, Ordering::Relaxed);
                        entregues = devidos;
                    }
                    if evento.is_none() {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    continue;
                }

                while pacote > 0 {
                    let mut dados: *mut u8 = std::ptr::null_mut();
                    let mut frames = 0u32;
                    let mut flags = 0u32;
                    captura.GetBuffer(&mut dados, &mut frames, &mut flags, None, None)?;
                    if frames > 0 {
                        let mudo = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
                        let pcm = if mudo || dados.is_null() || forma == Forma::Desconhecida {
                            vec![0u8; frames as usize * bytes_por_frame]
                        } else {
                            let cru = std::slice::from_raw_parts(
                                dados,
                                frames as usize * canais as usize * bytes_por_amostra(forma),
                            );
                            para_estereo(cru, forma, canais)
                        };
                        let instante = (entregues * 10_000_000 / ritmo as u64) as i64;
                        entregues += frames as u64;
                        cursor.store(entregues, Ordering::Relaxed);
                        entrega(Bocado {
                            pcm,
                            instante,
                            duracao: (frames as u64 * 10_000_000 / ritmo as u64) as i64,
                        });
                    }
                    captura.ReleaseBuffer(frames)?;
                    pacote = captura.GetNextPacketSize()?;
                }
            }
            let _ = cliente.Stop();
            Ok(())
        }
    }

    /// Quem está a tocar, agora, e com que volume. Diagnóstico: quando sai som das
    /// colunas e ninguém sabe de onde, é isto que responde — o Windows guarda uma sessão
    /// de áudio por processo, com o pico de cada uma.
    pub fn quem_toca() -> Result<()> {
        use windows::core::Interface;
        use windows::Win32::Media::Audio::{IAudioSessionControl2, IAudioSessionManager2};
        use windows::Win32::System::Threading::{
            OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT,
            PROCESS_QUERY_LIMITED_INFORMATION,
        };
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let enumerador: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let dispositivo = enumerador.GetDefaultAudioEndpoint(eRender, eConsole)?;
            let gestor: IAudioSessionManager2 = dispositivo.Activate(CLSCTX_ALL, None)?;
            let sessoes = gestor.GetSessionEnumerator()?;
            let n = sessoes.GetCount()?;
            let mut fora = Vec::new();
            println!("[som] {n} sessões de áudio no dispositivo de saída:");
            for i in 0..n {
                let Ok(s) = sessoes.GetSession(i) else {
                    continue;
                };
                let Ok(s2) = s.cast::<IAudioSessionControl2>() else {
                    continue;
                };
                let pid = s2.GetProcessId().unwrap_or(0);
                let activa = s.GetState().map(|e| e.0).unwrap_or(0);
                let mut nome = String::from("?");
                if pid != 0 {
                    if let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
                        let mut buf = [0u16; 260];
                        let mut tam = buf.len() as u32;
                        if QueryFullProcessImageNameW(
                            h,
                            PROCESS_NAME_FORMAT(0),
                            windows::core::PWSTR(buf.as_mut_ptr()),
                            &mut tam,
                        )
                        .is_ok()
                        {
                            nome = String::from_utf16_lossy(&buf[..tam as usize]);
                        }
                        let _ = windows::Win32::Foundation::CloseHandle(h);
                    }
                }
                // estado 1 = a tocar agora; 0 = inactiva; 2 = terminada
                let estado = match activa {
                    1 => "A TOCAR",
                    0 => "parada ",
                    _ => "morta  ",
                };
                println!("  {estado} pid {pid:<7} {nome}");
                fora.push((estado.trim().to_string(), pid, nome));
            }
            let _ = &fora;
            Ok(())
        }
    }

    /// Quem está a tocar agora, para diagnóstico — a mesma coisa que o `--quem-toca`, mas
    /// devolvida em vez de impressa.
    pub fn sessoes() -> Vec<(String, u32, String)> {
        use windows::core::Interface;
        use windows::Win32::Media::Audio::{IAudioSessionControl2, IAudioSessionManager2};
        use windows::Win32::System::Threading::{
            OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT,
            PROCESS_QUERY_LIMITED_INFORMATION,
        };
        let mut fora = Vec::new();
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let Ok(enumerador) =
                CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL)
            else {
                return fora;
            };
            let Ok(d) = enumerador.GetDefaultAudioEndpoint(eRender, eConsole) else {
                return fora;
            };
            let Ok(gestor) = d.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else {
                return fora;
            };
            let Ok(lista) = gestor.GetSessionEnumerator() else {
                return fora;
            };
            for i in 0..lista.GetCount().unwrap_or(0) {
                let Ok(sc) = lista.GetSession(i) else {
                    continue;
                };
                let Ok(s2) = sc.cast::<IAudioSessionControl2>() else {
                    continue;
                };
                let pid = s2.GetProcessId().unwrap_or(0);
                let activa = sc.GetState().map(|e| e.0).unwrap_or(0);
                let mut nome = String::from("?");
                if pid != 0 {
                    if let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
                        let mut buf = [0u16; 260];
                        let mut tam = buf.len() as u32;
                        if QueryFullProcessImageNameW(
                            h,
                            PROCESS_NAME_FORMAT(0),
                            windows::core::PWSTR(buf.as_mut_ptr()),
                            &mut tam,
                        )
                        .is_ok()
                        {
                            nome = String::from_utf16_lossy(&buf[..tam as usize]);
                            if let Some(x) = nome.rfind(std::path::MAIN_SEPARATOR) {
                                nome = nome[x + 1..].to_string();
                            }
                        }
                        let _ = windows::Win32::Foundation::CloseHandle(h);
                    }
                }
                if activa == 1 {
                    fora.push(("a tocar".into(), pid, nome));
                }
            }
        }
        fora
    }

    /// Capta durante `ms` milissegundos e devolve o volume médio e o pico.
    ///
    /// Existe para o autoteste do ECO: mede-se com a app calada e outra vez com ela a
    /// tocar um tom. Se o loopback estiver a excluir-nos, os dois números são iguais.
    pub fn medir_curto(ms: u64) -> Result<(f64, i32, bool)> {
        let parar = Arc::new(AtomicBool::new(false));
        let p2 = parar.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(ms));
            p2.store(true, Ordering::Relaxed);
        });
        let mut pico = 0i32;
        let mut soma = 0f64;
        let mut n = 0u64;
        let mut sem_eco = false;
        captar(
            parar,
            std::time::Instant::now(),
            |f| sem_eco = f.sem_eco,
            |b| {
                for c in b.pcm.chunks_exact(2) {
                    let v = i16::from_le_bytes([c[0], c[1]]) as i32;
                    pico = pico.max(v.abs());
                    soma += (v as f64) * (v as f64);
                    n += 1;
                }
            },
            &std::sync::atomic::AtomicU64::new(0),
        )?;
        Ok((
            if n > 0 { (soma / n as f64).sqrt() } else { 0.0 },
            pico,
            sem_eco,
        ))
    }

    /// Só para medir: capta durante `segundos` e diz o que ouviu.
    pub fn medir(segundos: u64) -> Result<()> {
        let parar = Arc::new(AtomicBool::new(false));
        let p2 = parar.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(segundos));
            p2.store(true, Ordering::Relaxed);
        });
        let mut bocados = 0u64;
        let mut amostras = 0u64;
        let mut pico = 0i32;
        let mut soma: f64 = 0.0;
        let mut formato = None;
        let inicio = std::time::Instant::now();
        captar(
            parar,
            inicio,
            |f| formato = Some(f),
            |b| {
                bocados += 1;
                for c in b.pcm.chunks_exact(2) {
                    let v = i16::from_le_bytes([c[0], c[1]]) as i32;
                    pico = pico.max(v.abs());
                    soma += (v as f64) * (v as f64);
                    amostras += 1;
                }
            },
            &std::sync::atomic::AtomicU64::new(0),
        )?;
        let s = inicio.elapsed().as_secs_f64().max(0.001);
        let f = formato.ok_or_else(|| anyhow!("o dispositivo não disse o formato"))?;
        let rms = if amostras > 0 {
            (soma / amostras as f64).sqrt()
        } else {
            0.0
        };
        println!(
            "[som] {:.1}s: {} canais a {} Hz | {} bocados, {} amostras ({:.0}/s por canal, \
             esperadas {}) | pico {} rms {:.0} | {}",
            s,
            f.canais,
            f.ritmo,
            bocados,
            amostras,
            amostras as f64 / s / f.canais as f64,
            f.ritmo,
            pico,
            rms,
            if f.sem_eco {
                "SEM ECO (so o som dos outros processos)"
            } else {
                "COM ECO (mistura do dispositivo, inclui a nossa propria voz)"
            },
        );
        Ok(())
    }
}

#[cfg(not(windows))]
mod win {
    use super::{Bocado, Formato, Result};
    use anyhow::anyhow;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    pub fn captar(
        _parar: Arc<AtomicBool>,
        _origem: std::time::Instant,
        _anunciar: impl FnOnce(Formato),
        _entrega: impl FnMut(Bocado),
    ) -> Result<()> {
        Err(anyhow!(
            "o som do sistema por enquanto só existe no Windows"
        ))
    }

    pub fn formato() -> Result<Formato> {
        Err(anyhow!(
            "o som do sistema por enquanto só existe no Windows"
        ))
    }

    pub fn medir(_s: u64) -> Result<()> {
        Err(anyhow!(
            "o som do sistema por enquanto só existe no Windows"
        ))
    }

    pub fn quem_toca() -> Result<()> {
        Err(anyhow!(
            "o som do sistema por enquanto só existe no Windows"
        ))
    }

    pub fn sessoes() -> Vec<(String, u32, String)> {
        Vec::new()
    }

    pub fn medir_curto(_ms: u64) -> Result<(f64, i32, bool)> {
        Err(anyhow!(
            "o som do sistema por enquanto só existe no Windows"
        ))
    }
}

pub use win::{captar, formato, medir, medir_curto, quem_toca, sessoes};

/// Lança a captura numa thread própria, ancorada em `origem`.
pub fn arrancar(
    parar: Arc<AtomicBool>,
    origem: std::time::Instant,
    formato: Formato,
    mut entrega: impl FnMut(Bocado) + Send + 'static,
    // O que dizer a quem está a partilhar quando o som se cala. Sem isto, a única marca
    // era uma linha no registo — e a pessoa continuava a ouvir tudo nas suas colunas.
    aviso: impl Fn(String) + Send + 'static,
) {
    std::thread::spawn(move || {
        let cursor = AtomicU64::new(0);
        // O `anunciar` traz o formato REAL — o que o `abrir_cliente` conseguiu, e não o
        // que a sondagem adivinhou. É aqui, e só aqui, que se sabe se há eco.
        let avisa_eco = &aviso;
        match captar(
            parar.clone(),
            origem,
            |f: Formato| {
                if !f.sem_eco {
                    avisa_eco(
                        "Esta versão do Windows não deixa separar o som da app do resto,                          por isso a partilha vai levar de volta a voz de quem está na                          chamada. Podes desligar o som da transmissão na engrenagem."
                            .into(),
                    );
                }
            },
            &mut entrega,
            &cursor,
        ) {
            Ok(()) => {}
            Err(e) => {
                eprintln!("[som] a captura do som falhou: {e:?}; a faixa segue em silêncio");
                aviso(
                    "o dispositivo de som mudou ou deixou de responder — a partilha continua, mas daqui para a frente vai sem som"
                        .into(),
                );
                // A faixa de som JÁ FOI DECLARADA no contentor — o `moov` saiu antes disto
                // poder falhar, e não há como o voltar atrás. Uma faixa declarada e nunca
                // alimentada é pior do que silêncio: quem recebe fica à espera de amostras
                // que não vêm, e o leitor pode simplesmente parar.
                //
                // Portanto alimenta-se. O silêncio custa quase nada a comprimir e mantém a
                // promessa que o cabeçalho fez.
                silencio(
                    parar,
                    origem,
                    formato,
                    cursor.load(Ordering::Relaxed),
                    entrega,
                );
            }
        }
    });
}

/// Enche a faixa de som com silêncio, ao ritmo certo, até mandarem parar.
fn silencio(
    parar: Arc<AtomicBool>,
    origem: std::time::Instant,
    formato: Formato,
    // Onde a faixa já ia. Recomeçar em zero mandava amostras para trás no tempo, e uma
    // linha de tempo que anda para trás não é um som mau — é um leitor que pára. Já traz o
    // silêncio inicial do atraso lá dentro, por isso os instantes contam a partir de zero.
    ja_entregues: u64,
    mut entrega: impl FnMut(Bocado),
) {
    let ritmo = formato.ritmo.max(8000) as u64;
    let bytes_por_quadro = 4usize; // estéreo, 16 bits
    let mut entregues: u64 = ja_entregues;
    while !parar.load(Ordering::Relaxed) {
        let decorrido = origem.elapsed().as_secs_f64();
        let devidos = (decorrido * ritmo as f64) as u64;
        if devidos > entregues + ritmo / 50 {
            let faltam = (devidos - entregues) as usize;
            entrega(Bocado {
                pcm: vec![0u8; faltam * bytes_por_quadro],
                instante: (entregues * 10_000_000 / ritmo) as i64,
                duracao: (faltam as u64 * 10_000_000 / ritmo) as i64,
            });
            entregues = devidos;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[allow(dead_code)]
fn _usa(_: &Bocado, _: &Formato, _: &AtomicBool, _: &Ordering) {}
