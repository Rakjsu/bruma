//! A ponte entre a interface e o núcleo. Nenhuma chave privada atravessa esta fronteira:
//! o JavaScript pede ações, o Rust é que assina e cifra.

use data_encoding::HEXLOWER;
use serde::Serialize;
use spike_common::log as blog;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};

use crate::estado::{self, App};
use crate::modelo::{self, Canal, Carga, Convite, Membro, MensagemVista, TipoCanal};
use crate::rede::Rede;

#[derive(Serialize)]
pub struct VistaServidor {
    pub id: String,
    pub nome: String,
    pub canais: Vec<Canal>,
    pub membros: Vec<Membro>,
}

#[derive(Serialize)]
pub struct Vista {
    /// A chave pública é o ID. É também o endereço na rede — não há dois identificadores.
    pub chave: String,
    pub nome: String,
    pub servidores: Vec<VistaServidor>,
}

/// Os comandos devolvem `Result<_, String>` porque o Tauri precisa de algo serializável, e
/// porque a mensagem tem de ser legível para quem está a usar a app, não um `Debug` de erro.
type R<T> = Result<T, String>;

fn erro(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[tauri::command]
pub fn estado(app: State<Arc<App>>) -> R<Vista> {
    let servidores = app.servidores.lock().map_err(erro)?;
    let vistas = servidores
        .values()
        .map(|s| {
            let e = s.estado();
            VistaServidor {
                id: s.id.clone(),
                nome: if e.nome.is_empty() {
                    "sem nome".into()
                } else {
                    e.nome
                },
                canais: e.canais,
                membros: e.membros,
            }
        })
        .collect();
    Ok(Vista {
        chave: app.minha_chave(),
        nome: app.nome.lock().map_err(erro)?.clone(),
        servidores: vistas,
    })
}

#[tauri::command]
pub fn definir_nome(
    nome: String,
    app: State<Arc<App>>,
    rede: State<Arc<Rede>>,
    janela: AppHandle,
) -> R<()> {
    let nome = nome.trim().to_string();
    if nome.is_empty() {
        return Err("o nome não pode ficar vazio".into());
    }
    *app.nome.lock().map_err(erro)? = nome.clone();
    app.gravar_indice().map_err(erro)?;

    // Apresenta-se em todos os servidores onde já está, para os outros verem o nome novo.
    let mut difundir: Vec<(String, blog::Entry)> = Vec::new();
    {
        let mut servidores = app.servidores.lock().map_err(erro)?;
        for s in servidores.values_mut() {
            let e = s
                .escrever(
                    &app.ident.signing,
                    &Carga::Apresentar { nome: nome.clone() },
                )
                .map_err(erro)?;
            difundir.push((s.id.clone(), e));
        }
    }
    for (id, e) in difundir {
        rede.difundir(&id, e);
        let _ = janela.emit("servidor-mudou", &id);
    }
    Ok(())
}

#[tauri::command]
pub fn criar_servidor(
    nome: String,
    app: State<Arc<App>>,
    rede: State<Arc<Rede>>,
    janela: AppHandle,
) -> R<String> {
    let nome = nome.trim().to_string();
    if nome.is_empty() {
        return Err("dá um nome ao servidor".into());
    }
    let id = modelo::id_hex(&modelo::novo_id().map_err(erro)?);
    let chave = estado::nova_chave_de_servidor().map_err(erro)?;
    let log = blog::Log::load(estado::caminho_do_log(&id)).map_err(erro)?;

    let mut srv = estado::Servidor {
        id: id.clone(),
        chave,
        log,
        peers: Vec::new(),
    };

    // Um servidor recém-criado sem canais é uma sala vazia sem portas. Cria-se o mínimo
    // para ser utilizável desde o primeiro segundo.
    let meu_nome = app.nome.lock().map_err(erro)?.clone();
    let arranque = vec![
        Carga::NomeDoServidor { nome: nome.clone() },
        Carga::CriarCanal {
            id: modelo::id_hex(&modelo::novo_id().map_err(erro)?),
            nome: "geral".into(),
            tipo: TipoCanal::Texto,
        },
        Carga::CriarCanal {
            id: modelo::id_hex(&modelo::novo_id().map_err(erro)?),
            nome: "Sala de voz".into(),
            tipo: TipoCanal::Voz,
        },
    ];
    let mut entradas = Vec::new();
    for c in &arranque {
        entradas.push(srv.escrever(&app.ident.signing, c).map_err(erro)?);
    }
    if !meu_nome.is_empty() {
        entradas.push(
            srv.escrever(&app.ident.signing, &Carga::Apresentar { nome: meu_nome })
                .map_err(erro)?,
        );
    }

    app.servidores.lock().map_err(erro)?.insert(id.clone(), srv);
    app.gravar_indice().map_err(erro)?;

    for e in entradas {
        rede.difundir(&id, e);
    }
    let _ = janela.emit("servidor-mudou", &id);
    Ok(id)
}

#[tauri::command]
pub fn criar_canal(
    servidor: String,
    nome: String,
    tipo: String,
    app: State<Arc<App>>,
    rede: State<Arc<Rede>>,
    janela: AppHandle,
) -> R<()> {
    let nome = nome.trim().to_string();
    if nome.is_empty() {
        return Err("dá um nome ao canal".into());
    }
    let tipo = match tipo.as_str() {
        "voz" => TipoCanal::Voz,
        _ => TipoCanal::Texto,
    };
    let carga = Carga::CriarCanal {
        id: modelo::id_hex(&modelo::novo_id().map_err(erro)?),
        nome,
        tipo,
    };
    let entrada = {
        let mut servidores = app.servidores.lock().map_err(erro)?;
        let srv = servidores
            .get_mut(&servidor)
            .ok_or("esse servidor não existe aqui")?;
        srv.escrever(&app.ident.signing, &carga).map_err(erro)?
    };
    rede.difundir(&servidor, entrada);
    let _ = janela.emit("servidor-mudou", &servidor);
    Ok(())
}

#[tauri::command]
pub fn apagar_canal(
    servidor: String,
    canal: String,
    app: State<Arc<App>>,
    rede: State<Arc<Rede>>,
    janela: AppHandle,
) -> R<()> {
    let entrada = {
        let mut servidores = app.servidores.lock().map_err(erro)?;
        let srv = servidores
            .get_mut(&servidor)
            .ok_or("esse servidor não existe aqui")?;
        srv.escrever(&app.ident.signing, &Carga::ApagarCanal { id: canal })
            .map_err(erro)?
    };
    rede.difundir(&servidor, entrada);
    let _ = janela.emit("servidor-mudou", &servidor);
    Ok(())
}

#[tauri::command]
pub fn criar_convite(servidor: String, app: State<Arc<App>>, rede: State<Arc<Rede>>) -> R<String> {
    let servidores = app.servidores.lock().map_err(erro)?;
    let srv = servidores
        .get(&servidor)
        .ok_or("esse servidor não existe aqui")?;
    let convite = Convite {
        servidor: srv.id.clone(),
        nome: srv.estado().nome,
        chave: HEXLOWER.encode(&srv.chave),
        anfitriao: rede.id().to_string(),
    };
    convite.codificar().map_err(erro)
}

#[tauri::command]
pub async fn entrar_com_convite(
    codigo: String,
    app: State<'_, Arc<App>>,
    rede: State<'_, Arc<Rede>>,
    janela: AppHandle,
) -> R<String> {
    let convite = Convite::descodificar(&codigo).map_err(erro)?;
    let chave = estado::hex32(&convite.chave).map_err(erro)?;

    let ja_tinha = {
        let servidores = app.servidores.lock().map_err(erro)?;
        servidores.contains_key(&convite.servidor)
    };

    if !ja_tinha {
        let log = blog::Log::load(estado::caminho_do_log(&convite.servidor)).map_err(erro)?;
        let mut srv = estado::Servidor {
            id: convite.servidor.clone(),
            chave,
            log,
            peers: vec![convite.anfitriao.clone()],
        };
        // Apresentar-se é o que faz a pessoa aparecer na lista de membros dos outros.
        let meu_nome = app.nome.lock().map_err(erro)?.clone();
        let entrada = if meu_nome.is_empty() {
            None
        } else {
            Some(
                srv.escrever(&app.ident.signing, &Carga::Apresentar { nome: meu_nome })
                    .map_err(erro)?,
            )
        };
        app.servidores
            .lock()
            .map_err(erro)?
            .insert(convite.servidor.clone(), srv);
        app.gravar_indice().map_err(erro)?;
        if let Some(e) = entrada {
            rede.difundir(&convite.servidor, e);
        }
    }

    // Ligar ao anfitrião é o que traz o histórico. Sem isto entra-se num servidor vazio.
    let app_arc: Arc<App> = (*app).clone();
    let rede_arc: Arc<Rede> = (*rede).clone();
    crate::rede::ligar(&rede_arc, &app_arc, &janela, &convite.anfitriao)
        .await
        .map_err(erro)?;

    let _ = janela.emit("servidor-mudou", &convite.servidor);
    Ok(convite.servidor)
}

#[tauri::command]
pub fn enviar(
    servidor: String,
    canal: String,
    texto: String,
    app: State<Arc<App>>,
    rede: State<Arc<Rede>>,
    janela: AppHandle,
) -> R<()> {
    let texto = texto.trim().to_string();
    if texto.is_empty() {
        return Ok(());
    }
    let entrada = {
        let mut servidores = app.servidores.lock().map_err(erro)?;
        let srv = servidores
            .get_mut(&servidor)
            .ok_or("esse servidor não existe aqui")?;
        srv.escrever(&app.ident.signing, &Carga::Mensagem { canal, texto })
            .map_err(erro)?
    };
    rede.difundir(&servidor, entrada);
    let _ = janela.emit("servidor-mudou", &servidor);
    Ok(())
}

#[tauri::command]
pub fn mensagens(servidor: String, canal: String, app: State<Arc<App>>) -> R<Vec<MensagemVista>> {
    let servidores = app.servidores.lock().map_err(erro)?;
    let srv = servidores
        .get(&servidor)
        .ok_or("esse servidor não existe aqui")?;
    Ok(srv.mensagens(&canal))
}

/// Anuncia a toda a gente que entrei (ou saí) de um canal de voz.
///
/// A presença é deliberadamente EFÉMERA: não vai para o log. Quem está numa sala agora não
/// é história, é estado — e escrever isso no log encheria o histórico de ruído que ninguém
/// quer reler.
#[tauri::command]
pub fn presenca_de_voz(servidor: String, canal: Option<String>, rede: State<Arc<Rede>>) -> R<()> {
    rede.anunciar_presenca(&servidor, canal);
    Ok(())
}

/// Encaminha sinalização WebRTC para UM peer.
///
/// O `dados` é opaco aqui: é SDP ou candidatos ICE que só a webview sabe ler. O Rust nunca
/// os interpreta, só os entrega a quem é destinado.
#[tauri::command]
pub fn enviar_sinal(
    para: String,
    servidor: String,
    canal: String,
    dados: String,
    rede: State<Arc<Rede>>,
) -> R<()> {
    rede.enviar_sinal(&para, &servidor, &canal, dados);
    Ok(())
}

/// Quem sou eu na rede — os peers identificam-se por este valor na sinalização.
#[tauri::command]
pub fn meu_endereco(rede: State<Arc<Rede>>) -> R<String> {
    Ok(rede.id().to_string())
}

/// Diagnóstico honesto: quantas entradas há e quantas estão sem pai.
/// Enquanto houver órfãs, o histórico tem buracos e a interface deve dizê-lo.
#[tauri::command]
pub fn saude(servidor: String, app: State<Arc<App>>) -> R<serde_json::Value> {
    let servidores = app.servidores.lock().map_err(erro)?;
    let srv = servidores
        .get(&servidor)
        .ok_or("esse servidor não existe aqui")?;
    Ok(serde_json::json!({
        "entradas": srv.log.len(),
        "orfas": srv.log.orfas().len(),
        "peers": srv.peers.len(),
    }))
}

/// O que a webview desta máquina consegue fazer, dito por ela e escrito no arranque.
///
/// A partilha de ecrã vai passar a ser captada e codificada em Rust e descodificada aqui
/// com o WebCodecs (Spike 4). Só que "a WebView2 tem WebCodecs" é uma suposição sobre a
/// versão que cada pessoa tem instalada, e a resposta muda de máquina para máquina — a
/// aceleração por hardware ainda mais. Portanto pergunta-se, e fica escrito.
#[tauri::command]
pub fn capacidades(linha: String) {
    println!("  capacidades: {linha}");
}

/* ==========================================================================
Partilha de ecrã nativa.

O ecrã é captado e codificado em Rust (ver `ecra.rs`) e sai em pedaços de MP4
fragmentado. Os mesmos pedaços vão a dois sítios: para a webview de quem partilha, que
os mostra como pré-visualização, e para a rede, um a um, só para quem carregou em
"Assistir".

Dois canais, e são mesmo diferentes: quem partilha só precisa de ver o seu, e quem
assiste precisa de saber de QUEM é o pedaço que chegou — daí o cabeçalho com a chave.
========================================================================== */

use std::sync::Mutex as SyncMutex;
use tauri::ipc::{Channel, InvokeResponseBody};

#[derive(Default)]
pub struct Ecra {
    pub estado: SyncMutex<crate::ecra::Estado>,
    /// Onde a webview quer receber o ecrã dos outros.
    pub entrada: SyncMutex<Option<Channel<InvokeResponseBody>>>,
    /// Onde ela quer ver o seu próprio ecrã — e SE está mesmo a ver.
    ///
    /// O booleano vive dentro do mesmo Mutex de propósito: o reenvio do cabeçalho e a
    /// abertura da torneira têm de ser um gesto só, senão um fragmento escapa entre os
    /// dois e chega antes do princípio — que é precisamente o bug que isto corrige.
    pub propria: SyncMutex<Option<(Channel<InvokeResponseBody>, bool)>>,
    /// A que servidor e canal pertence a partilha a decorrer.
    pub onde: SyncMutex<Option<(String, String)>>,
    /// O princípio da transmissão: o nome do codec e o segmento de inicialização.
    ///
    /// Saem uma única vez, quando a partilha começa. Quem carregar em "Assistir" a seguir
    /// — que é o caso normal — só apanharia fragmentos soltos, e um fragmento sem o
    /// cabeçalho não é vídeo nenhum: chegam bytes e não aparece imagem. Guardam-se para
    /// se mandarem a cada espectador novo antes de tudo o resto.
    pub cabecalho: SyncMutex<Vec<Vec<u8>>>,
}

/// Guarda-se o canal por onde o ecrã dos outros vai entrar. Um só, porque só se assiste a
/// uma pessoa de cada vez — e é o cabeçalho de cada pedaço que diz de quem é.
#[tauri::command]
pub fn receber_ecra(canal: Channel<InvokeResponseBody>, ecra: State<Arc<Ecra>>) {
    *ecra.entrada.lock().unwrap() = Some(canal);
}

/* ==========================================================================
A câmara.

Vai pelo mesmo transporte do ecrã e é codificada onde a voz é codificada — na interface,
com WebCodecs. A escolha tem uma razão: ao contrário do ecrã, que é um de cada vez, as
câmaras são VÁRIAS ao mesmo tempo, e é o navegador que já sabe abrir dispositivos,
descodificar em paralelo e desenhar. Trazer isso para Rust era reescrever o que já existe.

E há uma diferença que importa para a privacidade: o `getUserMedia` de uma câmara não faz
o WebView2 desenhar a barra "está a partilhar" — essa é só para captura de ECRÃ. Foi por
causa dela que o ecrã teve de sair do navegador; a câmara pode ficar.
========================================================================== */

/// Onde a câmara dos outros entra. Separado do ecrã porque são coisas diferentes na
/// interface: o ecrã é um só e enche o painel; as câmaras são muitas e são pequenas.
#[tauri::command]
pub fn receber_camara(canal: Channel<InvokeResponseBody>, camara: State<Arc<Camara>>) {
    *camara.entrada.lock().unwrap() = Some(canal);
}

#[derive(Default)]
pub struct Camara {
    pub entrada: SyncMutex<Option<Channel<InvokeResponseBody>>>,
}

pub static CAMARA: std::sync::OnceLock<Arc<Camara>> = std::sync::OnceLock::new();

/// Manda um pedaço de câmara a quem está na sala.
///
/// Como na voz, é a interface que diz a quem — é ela que sabe quem está na chamada, e
/// duplicar essa lista no Rust seria ter duas versões da mesma verdade.
#[tauri::command]
pub fn enviar_camara(
    para: Vec<String>,
    servidor: String,
    canal: String,
    dados: Vec<u8>,
    rede: State<Arc<Rede>>,
) {
    rede.enviar_camara(&para, &servidor, &canal, Arc::new(dados));
}

/// Chamado pela camada de rede a cada pedaço de câmara que chega.
pub fn camara_recebida(peer: &str, dados: Vec<u8>) {
    let Some(camara) = CAMARA.get() else { return };
    let Some(canal) = camara.entrada.lock().unwrap().clone() else {
        return;
    };
    let chave = peer.as_bytes();
    let n = chave.len().min(255);
    let mut corpo = Vec::with_capacity(1 + n + dados.len());
    corpo.push(n as u8);
    corpo.extend_from_slice(&chave[..n]);
    corpo.extend_from_slice(&dados);
    let _ = canal.send(InvokeResponseBody::Raw(corpo));
}

#[tauri::command]
// Oito argumentos porque um comando Tauri recebe os campos do invoke um a um; agrupar
// em struct só empurrava a contagem para outro sítio sem tornar nada mais claro.
#[allow(clippy::too_many_arguments)]
pub fn comecar_a_partilhar(
    servidor: String,
    canal_voz: String,
    fonte: String,
    altura: u32,
    fps: u32,
    debito: u32,
    com_som: bool,
    saida: Channel<InvokeResponseBody>,
    ecra: State<Arc<Ecra>>,
    rede: State<Arc<Rede>>,
    app: tauri::AppHandle,
) -> R<serde_json::Value> {
    let ecra = ecra.inner().clone();
    let rede = rede.inner().clone();
    *ecra.propria.lock().unwrap() = Some((saida, false));
    *ecra.onde.lock().unwrap() = Some((servidor.clone(), canal_voz.clone()));

    ecra.cabecalho.lock().unwrap().clear();
    let eco = ecra.clone();
    let entrega: crate::ecra::Entrega = Arc::new(move |pedaco: &[u8]| {
        // Os dois primeiros pedaços são o codec e o segmento de inicialização.
        {
            let mut c = eco.cabecalho.lock().unwrap();
            if c.len() < 2 {
                c.push(pedaco.to_vec());
            }
        }
        // A pré-visualização local só anda quando há alguém a olhar para ela. Enviar
        // sempre parecia inofensivo, mas era o bug do ecrã preto: os pedaços fluíam
        // desde o início, a fila do lado de lá aparava os antigos para não crescer, e
        // quando a pessoa carregava em "ver o meu ecrã" o cabeçalho já tinha ido fora.
        if let Some((c, a_ver)) = eco.propria.lock().unwrap().as_ref() {
            if *a_ver {
                let _ = c.send(InvokeResponseBody::Raw(pedaco.to_vec()));
            }
        }
        // E para a rede, um Arc partilhado por todos os espectadores.
        let espectadores = {
            let e = eco.estado.lock().unwrap();
            let v = e.espectadores.lock().unwrap();
            v.clone()
        };
        if !espectadores.is_empty() {
            rede.enviar_video(
                &espectadores,
                &servidor,
                &canal_voz,
                Arc::new(pedaco.to_vec()),
            );
        }
    });

    let alvo = crate::ecra::Alvo::analisar(&fonte).map_err(erro)?;
    let qualidade = crate::ecra::Qualidade {
        max_altura: altura,
        fps,
        debito,
        com_som,
    };
    // A queixa vai por evento, e não pelo valor de retorno: quando ela existir, este
    // comando já respondeu há muito. Ver `ecra::Queixa`.
    let queixa: crate::ecra::Queixa = {
        let app = app.clone();
        Arc::new(move |razao: String| {
            use tauri::Emitter;
            let _ = app.emit("partilha-falhou", razao);
        })
    };
    let aviso: crate::ecra::Aviso = {
        let app = app.clone();
        Arc::new(move |razao: String| {
            use tauri::Emitter;
            let _ = app.emit("partilha-aviso", razao);
        })
    };
    let (largura, altura) = {
        let mut e = ecra.estado.lock().map_err(erro)?;
        crate::ecra::comecar(&mut e, alvo, qualidade, entrega, queixa, aviso).map_err(erro)?
    };
    Ok(serde_json::json!({ "largura": largura, "altura": altura }))
}

#[tauri::command]
pub fn parar_de_partilhar(ecra: State<Arc<Ecra>>) {
    if let Ok(mut e) = ecra.estado.lock() {
        crate::ecra::parar(&mut e);
    }
    *ecra.propria.lock().unwrap() = None;
    *ecra.onde.lock().unwrap() = None;
}

/// Começa (ou retoma) a pré-visualização do próprio ecrã.
///
/// Primeiro o princípio da transmissão, depois abre-se a torneira — dentro do mesmo
/// lock, para nenhum fragmento se meter entre os dois.
#[tauri::command]
pub fn ver_meu_ecra(ecra: State<Arc<Ecra>>) {
    let mut guarda = ecra.propria.lock().unwrap();
    if let Some((c, a_ver)) = guarda.as_mut() {
        for p in ecra.cabecalho.lock().unwrap().iter() {
            let _ = c.send(InvokeResponseBody::Raw(p.clone()));
        }
        *a_ver = true;
    }
}

#[tauri::command]
pub fn parar_de_ver_meu_ecra(ecra: State<Arc<Ecra>>) {
    if let Some((_, a_ver)) = ecra.propria.lock().unwrap().as_mut() {
        *a_ver = false;
    }
}

/// Quem está mesmo a ver. Enquanto esta lista estiver vazia, nada sai desta máquina —
/// codifica-se e deita-se fora, que é mais barato do que parar e recomeçar o codificador
/// a cada espectador que chega ou sai.
#[tauri::command]
pub fn definir_espectadores(chaves: Vec<String>, ecra: State<Arc<Ecra>>, rede: State<Arc<Rede>>) {
    let Ok(e) = ecra.estado.lock() else { return };
    let mut lista = e.espectadores.lock().unwrap();

    // Quem é novo leva primeiro o princípio da transmissão. Sem isto vê os fragmentos
    // chegarem e um ecrã preto — que foi exatamente o que aconteceu no teste de par.
    let novos: Vec<String> = chaves
        .iter()
        .filter(|c| !lista.contains(c))
        .cloned()
        .collect();
    *lista = chaves;
    drop(lista);

    if novos.is_empty() {
        return;
    }
    let Some((servidor, canal)) = ecra.onde.lock().unwrap().clone() else {
        return;
    };
    for pedaco in ecra.cabecalho.lock().unwrap().iter() {
        rede.enviar_video(&novos, &servidor, &canal, Arc::new(pedaco.clone()));
    }
}

/// Chamado pela camada de rede quando chega um pedaço do ecrã de alguém.
///
/// O pedaço segue para a webview com a chave de quem o mandou à frente, para o JavaScript
/// saber a que transmissão pertence sem ter de adivinhar pela ordem de chegada.
pub fn ecra_recebido(peer: &str, _servidor: &str, _canal: &str, dados: Vec<u8>) {
    let Some(ecra) = ECRA.get() else { return };
    let Some(canal) = ecra.entrada.lock().unwrap().clone() else {
        return;
    };
    let chave = peer.as_bytes();
    let mut corpo = Vec::with_capacity(1 + chave.len() + dados.len());
    corpo.push(chave.len().min(255) as u8);
    corpo.extend_from_slice(&chave[..chave.len().min(255)]);
    corpo.extend_from_slice(&dados);
    let _ = canal.send(InvokeResponseBody::Raw(corpo));
}

/// A camada de rede não tem um `State` do Tauri à mão, e passar o `Ecra` por toda a
/// cadeia de chamadas só para isto tornava tudo pior. Fica aqui, escrito uma vez no
/// arranque e só lido depois.
pub static ECRA: std::sync::OnceLock<Arc<Ecra>> = std::sync::OnceLock::new();

/// `bruma --autoteste` põe a interface a partilhar o ecrã sozinha, a receber os pedaços e
/// a dizer o que viu.
///
/// Existe porque o caminho novo não se consegue verificar de outra maneira sem alguém à
/// frente do ecrã: os pedaços nascem em Rust, atravessam o IPC, entram num MediaSource e
/// só existem mesmo se um `<video>` os aceitar. Um teste em Rust prova metade; este prova
/// a cadeia toda, e a captura nativa — ao contrário do `getDisplayMedia` — não precisa de
/// um clique para arrancar.
/// Quantos segundos deve durar o autoteste, ou 0 se não foi pedido.
/// `--autoteste` faz 6 segundos; `--autoteste=30` faz trinta — útil para ver se a memória
/// se aguenta numa partilha longa, que é coisa que seis segundos nunca mostram.
#[tauri::command]
pub fn autoteste_pedido() -> u32 {
    for a in std::env::args() {
        if a == "--autoteste" {
            return 6;
        }
        if let Some(n) = a.strip_prefix("--autoteste=") {
            return n.parse().unwrap_or(6);
        }
    }
    0
}

/// A altura que o autoteste deve pedir: `--altura=1440`. Como o `--fps`, existe para se
/// poder PROVAR que a escolha muda o que sai — e a prova é ler as dimensões no MP4
/// produzido, com um leitor que não seja o nosso.
#[tauri::command]
pub fn autoteste_altura() -> u32 {
    for a in std::env::args() {
        if let Some(n) = a.strip_prefix("--altura=") {
            return n.parse().unwrap_or(720);
        }
    }
    720
}

/// As 24 palavras que recuperam esta identidade.
///
/// Não se guardam em lado nenhum nem se escrevem no registo: são derivadas da semente
/// sempre que alguém as pede, e vivem só o tempo de estarem no ecrã.
#[tauri::command]
pub fn palavras_da_identidade(nucleo: State<Arc<crate::estado::App>>) -> R<String> {
    spike_common::crypto::semente_em_palavras(nucleo.semente_bruta()).map_err(erro)
}

/// Substitui a identidade desta máquina pela que as palavras descrevem.
///
/// # Porque é que isto não mexe nos dados
///
/// A identidade nova não decifra os servidores da antiga — as chaves deles estão no índice,
/// cifrado com a semente ANTIGA. Portanto o índice antigo é posto de lado em vez de ser
/// apagado, e a app arranca limpa com a identidade recuperada. Quem estava numa sala volta
/// a entrar por convite, que é o caminho normal.
///
/// Apagar seria mais arrumado e muito pior: se a pessoa se enganou nas palavras, o que
/// tinha desaparecia sem volta.
#[tauri::command]
pub fn restaurar_identidade(palavras: String) -> R<String> {
    let semente = spike_common::crypto::palavras_em_semente(&palavras).map_err(erro)?;
    let raiz = crate::estado::raiz();
    let indice = raiz.join("indice.json");
    if indice.exists() {
        let guardado = indice.with_extension("json.antes-de-restaurar");
        std::fs::rename(&indice, &guardado).map_err(erro)?;
    }
    std::fs::write(raiz.join("identidade.key"), semente).map_err(erro)?;
    Ok("A identidade foi restaurada. Fecha e volta a abrir o Bruma.".into())
}

/// Mede o som que sai das colunas durante `ms`, e diz se a captura nos exclui.
///
/// Serve o autoteste do eco: mede-se calado e outra vez com a app a tocar um tom. Se o
/// loopback de processo estiver a funcionar, o tom que a app toca NÃO aparece aqui.
#[tauri::command]
pub fn medir_som(ms: u64) -> R<serde_json::Value> {
    let (rms, pico, sem_eco) = crate::som::medir_curto(ms).map_err(erro)?;
    let quem: Vec<String> = crate::som::sessoes()
        .into_iter()
        .map(|(_, pid, nome)| format!("{nome}#{pid}"))
        .collect();
    Ok(
        serde_json::json!({ "rms": rms, "pico": pico, "semEco": sem_eco, "quem": quem,
                           "eu": std::process::id() }),
    )
}

/// A fonte que o autoteste deve partilhar: `--fonte=ecra:99` aponta para um ecrã que não
/// existe, e serve para PROVAR o caminho da falha — que é o que nunca se testa e o que
/// aparece sempre na máquina dos outros.
#[tauri::command]
pub fn autoteste_fonte() -> String {
    for a in std::env::args() {
        if let Some(n) = a.strip_prefix("--fonte=") {
            return n.to_string();
        }
    }
    "ecra:1".into()
}

/// O ritmo que o autoteste deve pedir: `--fps=15`. Existe para se poder PROVAR que o
/// número escolhido no menu muda o que sai — medindo o mesmo ecrã a ritmos diferentes.
#[tauri::command]
pub fn autoteste_fps() -> u32 {
    for a in std::env::args() {
        if let Some(n) = a.strip_prefix("--fps=") {
            return n.parse().unwrap_or(30);
        }
    }
    30
}

/* ==========================================================================
Voz.

Vai pelo mesmo caminho do ecrã — o iroh — e não por WebRTC. A diferença prática é que
deixa de ser preciso configurar seja o que for: sem STUN, sem TURN, sem colar endereços
de servidores em duas máquinas. E, como o ecrã, deixa também de expor o endereço de
quem fala, porque não há um segundo túnel a furar o router por fora.

O som é codificado e descodificado na webview, com o Opus que ela já traz. O Rust não
olha para dentro dos pedaços: só os leva de um lado ao outro.
========================================================================== */

#[derive(Default)]
pub struct Voz {
    /// Onde a webview quer receber o som dos outros.
    pub entrada: SyncMutex<Option<Channel<InvokeResponseBody>>>,
}

#[tauri::command]
pub fn receber_voz(canal: Channel<InvokeResponseBody>, voz: State<Arc<Voz>>) {
    *voz.entrada.lock().unwrap() = Some(canal);
}

/// Um pedaço de som para cada pessoa da sala.
///
/// A interface é que diz a quem, porque é ela que sabe quem está na chamada. O Rust não
/// guarda essa lista para não haver duas versões da mesma verdade.
#[tauri::command]
pub fn enviar_voz(para: Vec<String>, dados: Vec<u8>, rede: State<Arc<Rede>>) {
    rede.enviar_voz(&para, &dados);
}

/// Chamado pela camada de rede a cada datagrama de voz que chega.
///
/// Vai para a webview com a chave de quem falou à frente, como o ecrã: sem isso não havia
/// como saber a que pessoa pertence o som, e numa sala com várias a falar ao mesmo tempo
/// isso é a diferença entre ouvir uma conversa e ouvir uma confusão.
pub fn voz_recebida(peer: &str, dados: &[u8]) {
    let Some(voz) = VOZ.get() else { return };
    let Some(canal) = voz.entrada.lock().unwrap().clone() else {
        return;
    };
    let chave = peer.as_bytes();
    let n = chave.len().min(255);
    let mut corpo = Vec::with_capacity(1 + n + dados.len());
    corpo.push(n as u8);
    corpo.extend_from_slice(&chave[..n]);
    corpo.extend_from_slice(dados);
    let _ = canal.send(InvokeResponseBody::Raw(corpo));
}

pub static VOZ: std::sync::OnceLock<Arc<Voz>> = std::sync::OnceLock::new();

/// A qualidade da ligação a cada pessoa, medida pelo próprio transporte.
///
/// Isto vinha das estatísticas do WebRTC. Agora vem do iroh, e diz mais: além do tempo de
/// ida e volta, sabe-se se a ligação é **direta** ou se está a passar por um relay — que é
/// a diferença entre o router ter sido furado ou não, e a coisa mais útil que se pode
/// mostrar a quem se está a queixar de que a chamada está má.
#[tauri::command]
pub fn qualidade(peers: Vec<String>, rede: State<Arc<Rede>>) -> Vec<serde_json::Value> {
    let Ok(ligacoes) = rede.ligacoes.lock() else {
        return Vec::new();
    };
    peers
        .iter()
        .filter_map(|p| {
            let c = ligacoes.get(p)?;
            let caminhos = c.paths();
            let escolhido = caminhos.iter().find(|x| x.is_selected());
            let (relay, ms) = match escolhido {
                Some(x) => (
                    x.is_relay(),
                    c.rtt(x.id())
                        .map(|d| d.as_secs_f64() * 1000.0)
                        .unwrap_or(0.0),
                ),
                None => (false, 0.0),
            };
            let n = rede
                .contagem
                .lock()
                .ok()
                .and_then(|c| c.get(p).copied())
                .unwrap_or_default();
            Some(serde_json::json!({
                "peer": p, "relay": relay, "ms": ms,
                "enviados": n.voz_env, "recebidos": n.voz_rec,
                "ecraEnviado": n.ecra_env, "ecraRecebido": n.ecra_rec,
            }))
        })
        .collect()
}

/// O autoteste de par: `""` significa "sou o anfitrião", um convite significa "sou o
/// convidado", e `None` significa que não foi pedido.
///
/// Existe porque a única parte da voz que nenhum teste de uma máquina só alcança é a do
/// meio: a lista de quem está na sala, o datagrama a atravessar, e o pedaço a chegar ao
/// descodificador do outro lado. Com duas instâncias emparelhadas isso passa a ser
/// verificável sem ninguém a clicar em nada.
#[tauri::command]
pub fn autoteste_par() -> Option<String> {
    for a in std::env::args() {
        if a == "--par" {
            return Some(String::new());
        }
        if let Some(c) = a.strip_prefix("--par=") {
            return Some(c.to_string());
        }
    }
    None
}

/// `bruma --medir-ui` faz a interface medir-se a si própria e escrever os números.
///
/// Existe porque fotografar a janela não serve para isto: o `PrintWindow` devolve a
/// WebView2 incompleta, e trazê-la à frente a partir de outro processo é bloqueado pelo
/// Windows. Perguntar ao DOM onde estão os elementos é exato e não depende de pixels.
#[tauri::command]
pub fn medir_ui_pedido() -> bool {
    std::env::args().any(|a| a == "--medir-ui")
}
