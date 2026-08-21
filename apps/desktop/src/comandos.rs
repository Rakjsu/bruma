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
