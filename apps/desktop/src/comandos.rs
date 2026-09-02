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
    /// Por canal: quantas por ler. Só os canais com alguma coisa aparecem aqui.
    pub nao_lidos: std::collections::BTreeMap<String, usize>,
}

#[derive(Serialize)]
pub struct VistaConversa {
    pub id: String,
    /// Quantas mensagens por ler nesta conversa.
    pub nao_lidos: usize,
    /// A chave da outra pessoa. Uma conversa é sempre entre duas.
    pub com: String,
    pub nome: String,
    /// O canal onde as mensagens desta conversa vivem.
    ///
    /// Vai daqui para a interface em vez de ser repetido lá: uma constante escrita nos dois
    /// lados é uma constante que um dia deixa de ser a mesma nos dois lados.
    pub canal: String,
}

#[derive(Serialize)]
pub struct Vista {
    /// A chave pública é o ID. É também o endereço na rede — não há dois identificadores.
    pub chave: String,
    pub nome: String,
    pub servidores: Vec<VistaServidor>,
    pub conversas: Vec<VistaConversa>,
}

/// Os comandos devolvem `Result<_, String>` porque o Tauri precisa de algo serializável, e
/// porque a mensagem tem de ser legível para quem está a usar a app, não um `Debug` de erro.
type R<T> = Result<T, String>;

/// O tecto de uma mensagem, em caracteres. Ver [`enviar`].
///
/// O mesmo número está no `app.js` como `MAX_TEXTO`, para o contador poder avisar antes de se
/// carregar em Enter. Dois sítios com o mesmo número é uma coisa que diverge — e a divergência
/// aqui é benigna nos dois sentidos: se o JS ficar maior, o Rust recusa e a interface mostra o
/// erro; se ficar menor, avisa cedo demais. Nenhum dos dois deixa passar o que não devia.
pub const MAX_TEXTO: usize = 4000;

/// E o do NOME, que é o mesmo `maxlength` do campo no `index.html`.
///
/// O `enviar` tinha tecto e o `definir_nome` não tinha nenhum do lado do Rust — só o
/// `maxlength="32"` do HTML, que é uma sugestão ao teclado e não um guarda. E um nome não é um
/// campo qualquer: o `definir_nome` escreve uma `Carga::Apresentar` no log de **todos** os
/// servidores onde a pessoa está. Um nome enorme punha em cada uma dessas salas uma entrada
/// que depois não cabe num quadro de sync — e a partir daí aquela sala deixava de sincronizar,
/// por uma coisa que a própria pessoa fez sem intenção nenhuma.
pub const MAX_NOME: usize = 32;

/// O nome que se aceita, e porquê — à parte do comando, para se poder medir.
///
/// O `definir_nome` precisa de `State<Arc<App>>` e de uma `AppHandle`, que não se constroem num
/// teste. Isto recebe uma `&str` e devolve o nome já aparado ou a razão de não servir, que é a
/// única parte com uma regra lá dentro.
fn nome_aceitavel(nome: &str) -> Result<String, String> {
    let nome = nome.trim();
    if nome.is_empty() {
        return Err("o nome não pode ficar vazio".into());
    }
    // Em CARACTERES, como no `enviar`: é o que a pessoa vê no campo, e contar bytes daria um
    // limite que encolhe quando se escreve com acentos — «Zé» gastaria três dos trinta e dois.
    if nome.chars().count() > MAX_NOME {
        return Err(format!(
            "esse nome tem {} caracteres e o limite é {MAX_NOME}",
            nome.chars().count()
        ));
    }
    Ok(nome.to_string())
}

fn erro(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[tauri::command]
pub fn estado(app: State<Arc<App>>) -> R<Vista> {
    let servidores = app.servidores.lock().map_err(erro)?;

    // Servidores e conversas vivem no mesmo mapa e são a mesma coisa por baixo. Aqui
    // separam-se, porque na interface não são a mesma coisa de todo: um servidor tem
    // canais, membros e um nome que alguém escolheu; uma conversa é uma pessoa.
    let mut vistas = Vec::new();
    let mut conversas = Vec::new();
    // Uma cópia, e o lock largado: o `nao_lidos` de cada servidor consulta este mapa dentro
    // do ciclo, e segurar dois locks durante um ciclo é como se fazem inversões de ordem.
    let lido = app.lido.lock().map_err(erro)?.clone();
    let eu = app.minha_chave();
    let mut nomes: std::collections::BTreeMap<String, String> = Default::default();

    for s in servidores.values() {
        let (e, entradas) = s.estado_e_entradas();
        for m in &e.membros {
            nomes
                .entry(m.chave.clone())
                .or_insert_with(|| m.nome.clone());
        }
        // Os canais que esta pessoa consegue mesmo abrir. Numa conversa há um só, e é
        // sintético; num servidor são os de TEXTO que existem agora.
        //
        // Há aqui um ovo e uma galinha: os canais saem do estado, e o estado sai da mesma
        // passagem que a contagem. Resolve-se pedindo o estado uma vez e derivando as duas
        // coisas dele — que é o que o `estado_e_por_ler` faz. Antes eram duas decifragens
        // completas do log por servidor, com o lock de TODOS os servidores preso durante as
        // duas; o por-ler tinha dobrado o custo do caminho mais quente da app.
        let contaveis: std::collections::BTreeSet<String> = if s.com.is_some() {
            [modelo::CANAL_DA_CONVERSA.to_string()]
                .into_iter()
                .collect()
        } else {
            e.canais
                .iter()
                .filter(|c| matches!(c.tipo, modelo::TipoCanal::Texto))
                .map(|c| c.id.clone())
                .collect()
        };
        let por_ler = s.por_ler_das_entradas(&entradas, &eu, &lido, &contaveis);
        match &s.com {
            None => vistas.push(VistaServidor {
                id: s.id.clone(),
                nome: if e.nome.is_empty() {
                    "sem nome".into()
                } else {
                    e.nome
                },
                canais: e.canais,
                membros: e.membros,
                nao_lidos: por_ler.into_iter().map(|(c, (n, _))| (c, n)).collect(),
            }),
            Some(com) => conversas.push((
                s.id.clone(),
                com.clone(),
                por_ler
                    .get(modelo::CANAL_DA_CONVERSA)
                    .map(|(n, _)| *n)
                    .unwrap_or(0),
            )),
        }
    }

    // O nome resolve-se depois de ver TODOS os servidores: numa conversa não há servidor
    // aberto de onde o tirar, e a pessoa pode ser de uma sala que não é a que está à frente.
    let conversas = conversas
        .into_iter()
        .map(|(id, com, nao_lidos)| {
            let nome = nomes
                .get(&com)
                .cloned()
                .unwrap_or_else(|| format!("{}…", &com[..6.min(com.len())]));
            VistaConversa {
                id,
                nao_lidos,
                com,
                nome,
                canal: modelo::CANAL_DA_CONVERSA.into(),
            }
        })
        .collect();

    Ok(Vista {
        chave: app.minha_chave(),
        nome: app.nome.lock().map_err(erro)?.clone(),
        servidores: vistas,
        conversas,
    })
}

/// A minha lista de pessoas conhecidas.
#[tauri::command]
pub fn amigos(app: State<Arc<App>>) -> R<Vec<estado::Amigo>> {
    Ok(app.amigos.lock().map_err(erro)?.clone())
}

/// Põe alguém na lista — pela chave, que é a única forma de o alcançar.
///
/// Não há directório onde procurar ninguém, e isso é uma propriedade a manter e não uma
/// lacuna: quem não tem a tua chave não te encontra, aconteça o que acontecer.
#[tauri::command]
pub fn adicionar_amigo(chave: String, nome: String, app: State<Arc<App>>) -> R<()> {
    app.adicionar_amigo(&chave, &nome).map_err(erro)
}

#[tauri::command]
pub fn remover_amigo(chave: String, app: State<Arc<App>>) -> R<()> {
    app.remover_amigo(chave.trim()).map_err(erro)
}

/// Marca que comparaste a chave com a pessoa por outro caminho.
#[tauri::command]
pub fn marcar_verificado(chave: String, verificado: bool, app: State<Arc<App>>) -> R<()> {
    app.marcar_verificado(chave.trim(), verificado)
        .map_err(erro)
}

/// Quem eu recuso, e a política de quem me pode escrever.
#[tauri::command]
pub fn permissoes(app: State<Arc<App>>) -> R<serde_json::Value> {
    Ok(serde_json::json!({
        "bloqueados": app.bloqueados.lock().map_err(erro)?.clone(),
        "quem_escreve": *app.quem_escreve.lock().map_err(erro)?,
    }))
}

/// Bloquear é também FECHAR o que já está aberto.
///
/// A primeira versão disto só escrevia na lista, e a lista só é consultada quando uma ligação
/// NOVA chega. Com uma sessão já aberta — que é o caso normal, porque quem incomoda alguém
/// costuma estar a falar com essa pessoa nesse momento — não acontecia nada: ele continuava a
/// escrever, e o som dele continuava a sair nas colunas, até a ligação cair sozinha. O QUIC
/// mantém-se vivo com keepalives; podia durar horas.
///
/// A interface dizia «bloqueado» e não estava. É o pior que uma definição de privacidade pode
/// fazer: dar a sensação em vez da coisa.
#[tauri::command]
pub fn bloquear(chave: String, sim: bool, app: State<Arc<App>>, rede: State<Arc<Rede>>) -> R<()> {
    aplicar_bloqueio(&app, &rede, &chave, sim).map_err(erro)
}

/// O bloqueio, fora do comando, para que a medição corra o MESMO código que o botão.
///
/// Se o teste chamasse só o `app.bloquear`, provava a lista e não o efeito — e o efeito é
/// precisamente a parte que faltava: guardar a chave numa lista não fecha a sessão que já
/// está aberta, e enquanto ela não fechar a pessoa continua a escrever e a pôr som nas
/// colunas.
pub fn aplicar_bloqueio(
    app: &Arc<App>,
    rede: &Arc<Rede>,
    chave: &str,
    sim: bool,
) -> anyhow::Result<()> {
    let chave = chave.trim().to_lowercase();
    app.bloquear(&chave, sim)?;
    if sim {
        if let Ok(l) = rede.ligacoes.lock() {
            if let Some((c, _)) = l.get(&chave) {
                // O `Drop` do guarda da sessão trata do resto: aborta as tarefas, tira-o do
                // mapa e avisa a interface. A sessão morre como morreria uma nova.
                // SEM RAZAO NENHUMA, e isso e a funcionalidade.
                //
                // O painel promete que ele nao distingue estar bloqueado de eu estar
                // desligado. Mas o QUIC leva a razao do `close` ate ao outro lado, e o
                // Bruma escreve o que lhe chega no registo: a palavra "bloqueado"
                // aterrava no bruma.log dele. A promessa era falsa e a app e que a
                // desmentia.
                //
                // Uma ligacao que fecha sem razao e indistinguivel de uma que caiu.
                c.close(0u32.into(), b"");
            }
        }
    }
    Ok(())
}

/// Fabrica convites venenosos para a medicao os experimentar. So serve para isso.
///
/// Fabrica-los no Rust e nao no JS e de proposito: usa-se o MESMO `Convite::codificar` que a
/// app usa, portanto o que se experimenta e um convite a serio e nao a minha ideia de como um
/// convite e feito. Um teste que constroi o seu proprio formato acaba a testar o construtor
/// do teste.
#[cfg(debug_assertions)]
#[tauri::command]
pub fn convites_de_teste() -> R<String> {
    let bom = "aa".repeat(32);
    let raiz = estado::raiz();
    let fora = raiz
        .parent()
        .map(|p| p.join("ESCAPOU"))
        .unwrap_or_else(|| std::path::PathBuf::from("ESCAPOU"));
    let casos: Vec<(&str, String, String)> = vec![
        (
            "absoluto",
            fora.to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/"),
            bom.clone(),
        ),
        ("subir", "../../ESCAPOU".into(), bom.clone()),
        ("barra", "sub/ESCAPOU".into(), bom.clone()),
        ("nulo", format!("abc{}def", '\0'), bom.clone()),
        ("vazio", String::new(), bom.clone()),
        // E o terceiro que ganhava direitos de sala sem ter provado nada, aqui com uma chave
        // que nem sequer e uma chave.
        ("anfitriao-lixo", "0".repeat(32), "nao-sou-uma-chave".into()),
    ];
    let mut saida: Vec<Vec<String>> = Vec::new();
    for (nome, servidor, anfitriao) in casos {
        let c = crate::modelo::Convite {
            servidor,
            nome: "isca".into(),
            chave: "bb".repeat(32),
            anfitriao,
        };
        if let Ok(codigo) = c.codificar() {
            saida.push(vec![nome.to_string(), codigo]);
        }
    }
    serde_json::to_string(&saida).map_err(erro)
}

/// Alguma coisa apareceu FORA da pasta de dados, ou com um nome que nao e um id nosso?
#[cfg(debug_assertions)]
#[tauri::command]
pub fn escapou_alguma_coisa(app: State<Arc<App>>) -> R<String> {
    let raiz = estado::raiz();
    let mut achados = Vec::new();
    if let Some(pai) = raiz.parent() {
        for cand in [pai.join("ESCAPOU"), pai.join("ESCAPOU.json")] {
            if cand.exists() {
                achados.push(cand.to_string_lossy().to_string());
            }
        }
    }
    if let Ok(rd) = std::fs::read_dir(raiz.join("servidores")) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            let base = n.trim_end_matches(".json");
            if !estado::id_de_servidor_valido(base) {
                achados.push(format!("ficheiro:{n}"));
            }
        }
    }
    // E o estado em memoria: um id valido na forma pode na mesma ter entrado por um convite
    // fabricado. O `00000...0` era exactamente esse caso -- passava pelo nome do ficheiro e
    // ficava la na mesma, com uma chave de anfitriao que nem sequer e uma chave.
    if let Ok(s) = app.servidores.lock() {
        for srv in s.values() {
            if let Some(c) = &srv.convidou {
                if !estado::chave_valida(c) {
                    achados.push(format!(
                        "convidou-invalido:{}",
                        &srv.id[..8.min(srv.id.len())]
                    ));
                }
            }
            for pr in &srv.peers {
                if !estado::chave_valida(pr) {
                    achados.push(format!("peer-invalido:{}", &srv.id[..8.min(srv.id.len())]));
                }
            }
        }
    }
    Ok(if achados.is_empty() {
        "nenhum".into()
    } else {
        achados.join(",")
    })
}

/// Marca um canal como lido até à sua última mensagem.
///
/// Devolve a hora até onde ESTAVA lido antes desta chamada — é isso que a interface usa para
/// desenhar a linha de «novas mensagens» no sítio certo. Se devolvesse o valor novo, a linha
/// aparecia sempre no fim e não servia para nada.
#[tauri::command]
pub fn marcar_lido(
    servidor: String,
    canal: String,
    marcar: Option<bool>,
    app: State<Arc<App>>,
) -> R<i64> {
    let antes = app.lido_ate(&servidor, &canal);
    // `marcar: false` só quer saber até onde estava lido, sem mexer em nada.
    //
    // Existe porque marcar como lido tem de depender de a janela estar À FRENTE: uma
    // mensagem que chega ao canal aberto enquanto eu estou noutra aplicação era marcada como
    // lida pelo redesenho e nunca chegava a gerar aviso nenhum — a app dava-a por vista sem
    // ninguém a ter visto. Mas a interface continua a precisar do valor para saber onde pôr
    // a linha de «novas mensagens».
    if marcar == Some(false) {
        return Ok(antes);
    }
    let ate = {
        let s = app.servidores.lock().map_err(erro)?;
        match s.get(&servidor) {
            Some(srv) => srv.ultima_mensagem(&canal, &app.minha_chave()),
            None => return Ok(antes),
        }
    };
    if ate > 0 && app.marcar_lido(&servidor, &canal, ate) {
        app.gravar_indice().map_err(erro)?;
    }
    Ok(antes)
}

#[tauri::command]
pub fn definir_quem_escreve(politica: String, app: State<Arc<App>>) -> R<()> {
    let p = match politica.as_str() {
        "todos" => estado::QuemEscreve::Todos,
        "salas" => estado::QuemEscreve::Salas,
        "amigos" => estado::QuemEscreve::Amigos,
        _ => return Err("não conheço essa definição".into()),
    };
    *app.quem_escreve.lock().map_err(erro)? = p;
    app.gravar_indice().map_err(erro)
}

/// Abre a conversa privada com alguém, ou devolve a que já existe.
///
/// Não há convite: o id e a chave saem das duas identidades. O que pode faltar é a chave de
/// conversa da outra pessoa, e nesse caso a resposta diz porquê — é preciso terem estado
/// ligados uma vez, e isso acontece sozinho assim que ambos estiverem online.
#[tauri::command]
pub fn abrir_conversa(
    peer: String,
    app: State<Arc<App>>,
    rede: State<Arc<Rede>>,
    janela: AppHandle,
) -> R<String> {
    let peer = peer.trim().to_string();
    let id = app.abrir_conversa(&peer).map_err(erro)?;

    // Apresentar-me, como se faz ao criar um servidor ou ao entrar num. Sem isto o outro
    // lado vê as minhas mensagens assinadas por uma chave sem nome — a `MensagemVista`
    // tira o nome dos membros do log, e num log acabado de nascer não há membros nenhuns.
    //
    // A condição é «nunca escrevi aqui», e não «a conversa é nova». Foi o meu primeiro
    // engano: quando é o outro lado que abre a conversa, ela chega-me já criada, eu saltava
    // a apresentação, e ficava a aparecer-lhe como uma chave sem nome — só de um dos lados,
    // que é o tipo de assimetria que passa despercebida.
    let minha = app.minha_chave();
    let ja_me_apresentei = {
        let s = app.servidores.lock().map_err(erro)?;
        s.get(&id).is_some_and(|x| x.log.escreveu(&minha))
    };
    if ja_me_apresentei {
        return Ok(id);
    }

    let meu_nome = app.nome.lock().map_err(erro)?.clone();
    let mut s = app.servidores.lock().map_err(erro)?;
    if let Some(srv) = s.get_mut(&id) {
        let entrada = srv
            .escrever(&app.ident.signing, &Carga::Apresentar { nome: meu_nome })
            .map_err(erro)?;
        rede.difundir(&id, entrada);
    }
    drop(s);
    let _ = janela.emit("servidor-mudou", &id);
    Ok(id)
}

#[tauri::command]
pub fn definir_nome(
    nome: String,
    app: State<Arc<App>>,
    rede: State<Arc<Rede>>,
    janela: AppHandle,
) -> R<()> {
    let nome = nome_aceitavel(&nome)?;
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

    let srv = estado::Servidor::novo(id.clone(), chave, log, Vec::new(), None, None);

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
    // A CHAVE DURÁVEL ANTES DO PRIMEIRO BYTE CIFRADO COM ELA (#13).
    //
    // Estava ao contrário: escrevia as quatro entradas de arranque — cada `escrever` faz um
    // `anexar` com `sync_data`, ou seja fica MESMO no disco — e só depois gravava o índice.
    // Uma queda de energia nessa janela deixava `servidores/{id}.json` com quatro entradas
    // que ninguém no mundo decifra, e o índice sem vestígio de que a sala existiu. A regra é:
    // a chave tem de ser durável antes de existir o primeiro dado cifrado com ela.
    app.servidores.lock().map_err(erro)?.insert(id.clone(), srv);
    app.gravar_indice().map_err(erro)?;

    // Só AGORA as entradas de arranque, no servidor que já está no mapa e com a chave gravada.
    let entradas = {
        let mut sv = app.servidores.lock().map_err(erro)?;
        let srv = sv.get_mut(&id).ok_or("o servidor desapareceu do mapa")?;
        let mut es = Vec::new();
        for c in &arranque {
            es.push(srv.escrever(&app.ident.signing, c).map_err(erro)?);
        }
        if !meu_nome.is_empty() {
            es.push(
                srv.escrever(&app.ident.signing, &Carga::Apresentar { nome: meu_nome })
                    .map_err(erro)?,
            );
        }
        es
    };

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

/// Apaga uma conversa e tudo o que ela arrasta (#87).
#[tauri::command]
pub fn apagar_conversa(id: String, app: State<Arc<App>>, janela: AppHandle) -> R<()> {
    app.apagar_conversa(&id).map_err(erro)?;
    let _ = janela.emit("servidor-mudou", &id);
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
    // Uma conversa não se convida: ela existe porque as duas chaves existem, e não há segredo
    // nenhum para pôr num convite. Cinto de segurança para o caso de a interface pedir.
    {
        let s = app.servidores.lock().map_err(erro)?;
        if s.get(&servidor).is_some_and(|x| x.com.is_some()) {
            return Err("uma conversa privada não tem convite".into());
        }
    }
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

    // NADA DE UM CONVITE VAI PARA UM CAMINHO DE FICHEIRO SEM SER OLHADO.
    //
    // O convite é JSON em base32 SEM assinatura: quem o escreve escolhe o que lá está. E o
    // `convite.servidor` ia directo para `caminho_do_log`, que faz
    // `raiz().join("servidores").join(format!("{id}.json"))`. O `join` com um caminho
    // absoluto deita fora o prefixo — um convite a apontar para a pasta de arranque do
    // Windows fazia a app escrever um ficheiro lá dentro. Com `..` chegava-se ao mesmo.
    //
    // E não bastava confiar no erro que o comando dava a seguir: ele dava mesmo, mas só ao
    // tentar ligar-se, DEPOIS de o ficheiro já estar criado. Quem lesse o erro concluía
    // «recusado».
    //
    // O erro é o mesmo para os dois casos: dizer «esse id não presta» a quem fabricou o
    // convite é dizer-lhe o que a app repara.
    if !estado::id_de_servidor_valido(&convite.servidor) {
        return Err("esse convite não é válido".into());
    }
    if !estado::chave_valida(&convite.anfitriao) {
        return Err("esse convite não é válido".into());
    }

    // NADA DE UM CONVITE VAI PARA UM CAMINHO DE FICHEIRO SEM SER OLHADO.
    //
    // O convite é JSON em base32 SEM assinatura: quem o escreve escolhe o que lá está. E o
    // `convite.servidor` ia directo para `caminho_do_log`, que faz
    // `raiz().join("servidores").join(format!("{id}.json"))`. O `join` com um caminho
    // absoluto deita fora o prefixo — um convite com o `servidor` a apontar para a pasta de
    // arranque do Windows fazia a app escrever um ficheiro lá dentro. Com `..` chegava-se ao
    // mesmo pelo caminho longo.
    //
    // O erro tem de ser o mesmo para os dois casos: dizer «esse id não presta» a quem
    // fabricou o convite é dizer-lhe o que a app repara.

    let ja_tinha = {
        let servidores = app.servidores.lock().map_err(erro)?;
        servidores.contains_key(&convite.servidor)
    };

    if !ja_tinha {
        let log = blog::Log::load(estado::caminho_do_log(&convite.servidor)).map_err(erro)?;
        // `convidou`, e NÃO `peers`.
        //
        // Estava a pôr o campo `anfitriao` de um convite não assinado na lista que decide
        // quem me pode pôr som nas colunas e forjar presença. Bastava alguém dar-me um
        // convite com a chave de um terceiro lá dentro para esse terceiro ganhar direitos de
        // sala sobre mim sem nunca ter provado nada — a terceira porta da mesma família,
        // depois do `aplicar` e do `aprender_dos_logs`.
        //
        // Como `convidou` ele continua a servir para o que precisa: discar-lhe e trocar o
        // histórico desta sala. Escreva ele uma entrada que decifra, e entra nos `peers`.
        let srv = estado::Servidor::novo(
            convite.servidor.clone(),
            chave,
            log,
            Vec::new(),
            Some(convite.anfitriao.clone()),
            None, // um servidor, nao uma conversa
        );
        // A CHAVE DURÁVEL ANTES DA PRIMEIRA ENTRADA CIFRADA (#13), como no `criar_servidor`:
        // a entrada `Apresentar` é escrita no disco por `srv.escrever`, e vinha antes do
        // `gravar_indice`. Insere-se e grava-se o índice primeiro; a apresentação só depois.
        let meu_nome = app.nome.lock().map_err(erro)?.clone();
        app.servidores
            .lock()
            .map_err(erro)?
            .insert(convite.servidor.clone(), srv);
        app.gravar_indice().map_err(erro)?;

        let entrada = if meu_nome.is_empty() {
            None
        } else {
            let mut sv = app.servidores.lock().map_err(erro)?;
            let srv = sv
                .get_mut(&convite.servidor)
                .ok_or("o servidor desapareceu do mapa")?;
            Some(
                srv.escrever(&app.ident.signing, &Carga::Apresentar { nome: meu_nome })
                    .map_err(erro)?,
            )
        };
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
    // O TECTO DE UMA MENSAGEM.
    //
    // Uma mensagem entra no log, vai para o disco dos dois lados, e é sincronizada em cada
    // ligação — para sempre. Não há apagar. Sem tecto, uma colagem distraída de cinco
    // megabytes fica lá, e o custo é de quem a recebe, que não escolheu nada.
    //
    // Aqui e não só na interface: uma verificação que só existe no JS é uma sugestão. E em
    // CARACTERES e não em bytes, porque é o que a pessoa vê no contador — contar bytes daria
    // um limite que encolhe quando se escreve com acentos, e ninguém percebia porquê.
    //
    // Isto não impede um par com software modificado de escrever mais: o que se guarda é o
    // que decifra, e o tamanho não é uma prova de nada. Está dito no README.
    if texto.chars().count() > MAX_TEXTO {
        return Err(format!(
            "essa mensagem tem {} caracteres e o limite é {MAX_TEXTO}",
            texto.chars().count()
        ));
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
    /// A GERAÇÃO da partilha: sobe a cada `parar_de_partilhar`.
    ///
    /// # A corrida que isto fecha
    ///
    /// `parar` não espera pelo codificador. A thread dele continua viva a drenar até
    /// quatro frames da fila e depois faz o `Finalize`, que despeja o que o codificador de
    /// hardware tinha em voo — e tudo isso sai pela closure `entrega` ANTIGA, que escreve
    /// neste mesmo `Ecra`. Se entretanto começou uma partilha nova, o `cabecalho` — que só
    /// verifica `len() < 2` — podia ficar com um segmento de MEDIA da partilha antiga no
    /// lugar do `moov` da nova, e cada espectador que entrasse recebia lixo em vez do
    /// segmento de inicialização, para toda a partilha.
    ///
    /// O recomeço não é automático — é um clique em «Transmitir» ou parar-e-escolher —, mas
    /// um clique é mais rápido do que um `Finalize` de hardware. A closure guarda a geração
    /// com que nasceu e cala-se se ela já não for a actual.
    pub geracao: std::sync::atomic::AtomicU64,
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
    // VERIFICAR PRIMEIRO, MUDAR DEPOIS (#44). Isto mudava `propria`, `onde` e limpava o
    // `cabecalho` ANTES de olhar para a fonte ou para o estado — e um `Err` a seguir
    // deixava a pré-visualização sem canal e o cabeçalho de uma partilha VIVA a ser
    // reenchido com os dois fragmentos seguintes de media. Um erro que devia ser inócuo
    // partia a partilha que já estava a correr.
    //
    // E o erro leva um CÓDIGO à frente, separado por `|`: a interface decide o que fazer
    // pelo código, nunca pelo texto — o dia em que alguém acentuar a frase, o seletor
    // deixava de reabrir em silêncio.
    let alvo = crate::ecra::Alvo::analisar(&fonte).map_err(|e| erro_de_partilha("fonte", e))?;
    if ecra.estado.lock().map_err(erro)?.a_partilhar() {
        return Err(erro_de_partilha(
            "ja-a-partilhar",
            anyhow::anyhow!("já estás a partilhar"),
        ));
    }
    *ecra.propria.lock().unwrap() = Some((saida, false));
    *ecra.onde.lock().unwrap() = Some((servidor.clone(), canal_voz.clone()));

    ecra.cabecalho.lock().unwrap().clear();
    let eco = ecra.clone();
    // A geração com que ESTA partilha nasce. Ver `Ecra::geracao`.
    let nascida = ecra.geracao.load(std::sync::atomic::Ordering::SeqCst);
    let entrega: crate::ecra::Entrega = Arc::new(move |pedaco: &[u8]| {
        // Os dois primeiros pedaços são o codec e o segmento de inicialização.
        {
            let mut c = eco.cabecalho.lock().unwrap();
            // UMA CLOSURE DE OUTRA GERAÇÃO NÃO ENTREGA NADA. A verificação vive DENTRO do
            // lock do `cabecalho`, e não antes: entre olhar e escrever podia entrar o
            // `parar` da partilha nova, e o segmento velho aterrava na mesma.
            if eco.geracao.load(std::sync::atomic::Ordering::SeqCst) != nascida {
                return;
            }
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
        Arc::new(move |chave: &'static str, texto: String| {
            use tauri::Emitter;
            // Com chave (#41): a interface guarda um aviso por chave, e um texto vazio
            // retira o dessa chave.
            let _ = app.emit(
                "partilha-aviso",
                serde_json::json!({ "chave": chave, "texto": texto }),
            );
        })
    };
    // O ritmo medido (#113), de segundo a segundo, por evento próprio.
    let ritmo: crate::ecra::Ritmo = {
        let app = app.clone();
        Arc::new(move |ips: f64, largados: u64, s: u64| {
            use tauri::Emitter;
            let _ = app.emit(
                "partilha-ritmo",
                serde_json::json!({ "ips": ips, "largados": largados, "s": s }),
            );
        })
    };
    let (largura, altura) = {
        let mut e = ecra.estado.lock().map_err(erro)?;
        match crate::ecra::comecar(&mut e, alvo, qualidade, entrega, queixa, aviso, ritmo) {
            Ok(t) => t,
            Err(e) => {
                // O que se mudou em cima desfaz-se: um `Err` aqui não pode deixar o estado
                // a dizer que há partilha.
                *ecra.propria.lock().unwrap() = None;
                *ecra.onde.lock().unwrap() = None;
                let codigo = {
                    let t = e.to_string();
                    if t.contains("já fechou") || t.contains("já não é a mesma") {
                        "janela-fechou"
                    } else if t.contains("sem esse ecrã") || t.contains("já não está ligado") {
                        "sem-ecra"
                    } else if t.contains("só existe no Windows") {
                        "so-windows"
                    } else {
                        "outra"
                    }
                };
                return Err(erro_de_partilha(codigo, e));
            }
        }
    };
    Ok(serde_json::json!({ "largura": largura, "altura": altura }))
}

/// Um erro de partilha com um CÓDIGO estável à frente (#44): `janela-fechou|essa janela já
/// fechou`. A interface separa no primeiro `|` e decide pelo código.
fn erro_de_partilha(codigo: &str, e: anyhow::Error) -> String {
    format!("{codigo}|{e}")
}

#[tauri::command]
pub fn parar_de_partilhar(ecra: State<Arc<Ecra>>) {
    // A geração sobe AQUI, antes de qualquer outra coisa: a partir deste instante, o que a
    // closure da partilha antiga ainda entregar já não é desta partilha. Ver `Ecra::geracao`.
    ecra.geracao
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    // `BRUMA_UI_SURDA=1`: a interface deixa de fazer a limpeza de volta, para se poder
    // provar que o Rust para o som SOZINHO quando a imagem morre (#40). So em debug.
    if crate::bandeiras::ui_surda() {
        eprintln!("[teste] parar_de_partilhar ignorado (BRUMA_UI_SURDA)");
        return;
    }
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
    // Quem chega leva o cabeçalho (abaixo) e, logo a seguir, um frame COMPLETO (#111).
    // Sem isto ficava «à espera da imagem…» até ao próximo frame completo natural — que
    // num ecrã parado podia estar a quinze segundos.
    e.pedir_chave();
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
#[cfg(debug_assertions)]
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

/// Onde ficam os dados, o registo e a versão — para as Definições poderem dizê-lo.
#[tauri::command]
pub fn sobre_esta_instalacao() -> serde_json::Value {
    serde_json::json!({
        "versao": env!("CARGO_PKG_VERSION"),
        "pasta": crate::estado::raiz().display().to_string(),
        "registo": crate::registo::caminho().display().to_string(),
        // O rasto do INSTALADOR, quando existe (#178). Vive sempre ao lado do exe — é o
        // instalador que o escreve —, enquanto o `registo` da app pode estar noutra pasta.
        // As Definições diziam «o rasto fica aqui, é a primeira coisa a olhar» a apontar
        // para um ficheiro onde o instalador nunca escreveu uma linha.
        "registo_do_instalador": std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|p| p.join("instalador.log")))
            .filter(|p| p.exists())
            .map(|p| p.display().to_string()),
    })
}

/// A actualização que morreu a meio, se a última morreu a meio (#121).
///
/// O instalador escreve um carimbo em `%APPDATA%\Bruma\actualizacao.json` antes de mexer
/// em seja o que for, e marca-o «pronto» no fim. Se a app arranca e o carimbo diz que uma
/// versão DIFERENTE ficou por instalar, é porque a actualização falhou — UAC recusado,
/// extracção morta — e até aqui ninguém dizia nada: a app reabria na versão antiga, calada.
///
/// Lê-se UMA vez e apaga-se: um aviso é um aviso, não um eco a cada arranque. E um carimbo
/// com mais de uma semana ignora-se — um resto esquecido não deve assustar ninguém.
#[tauri::command]
pub fn actualizacao_incompleta() -> Option<String> {
    let base = std::env::var_os("APPDATA")?;
    let caminho = std::path::PathBuf::from(base)
        .join("Bruma")
        .join("actualizacao.json");
    let texto = std::fs::read_to_string(&caminho).ok()?;
    let _ = std::fs::remove_file(&caminho);
    let j: serde_json::Value = serde_json::from_str(&texto).ok()?;
    if j["estado"].as_str() == Some("pronto") {
        return None;
    }
    let alvo = j["alvo"].as_str()?;
    // Se JÁ SOMOS a versão do carimbo, a instalação aconteceu — só o «pronto» se perdeu.
    if alvo == env!("CARGO_PKG_VERSION") {
        return None;
    }
    let instante = j["instante"].as_u64().unwrap_or(0);
    let agora = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    if agora.saturating_sub(instante) > 7 * 86_400 {
        return None;
    }
    Some(format!(
        "A última actualização (para a {alvo}) não chegou ao fim — continuas na {}.",
        env!("CARGO_PKG_VERSION")
    ))
}

/// Abre a pasta dos dados no Explorador.
///
/// Existe porque dizer à pessoa "vai a %APPDATA%\Bruma" é pedir-lhe que decore um caminho
/// para ir buscar um ficheiro de registo quando alguma coisa já correu mal. Um botão poupa
/// esse passo precisamente no momento em que ela tem menos paciência para ele.
#[tauri::command]
pub fn abrir_pasta_de_dados() -> R<()> {
    let raiz = crate::estado::raiz();
    std::process::Command::new("explorer")
        .arg(raiz.as_os_str())
        .spawn()
        .map_err(erro)?;
    Ok(())
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
pub fn restaurar_identidade(
    palavras: String,
    app: State<Arc<App>>,
    janela: AppHandle,
) -> R<String> {
    let semente = spike_common::crypto::palavras_em_semente(&palavras).map_err(erro)?;
    let raiz = crate::estado::raiz();
    let identidade = raiz.join("identidade.key");
    let indice = raiz.join("indice.json");

    // UM CARIMBO ÚNICO, e o MESMO para os dois (#15, #16, #74).
    //
    // A semente antiga tem de ser GUARDADA, não escrita por cima: o índice que se guarda «para
    // o caso de te enganares nas palavras» está cifrado com ela, e sem ela é lixo. E o nome
    // não pode ser fixo — restaurar duas vezes atropelava o primeiro guardado. O carimbo liga
    // a semente antiga ao índice antigo que ela decifra.
    let carimbo = crate::estado::agora_ms();

    // A semente antiga primeiro. Se falhar, aborta-se ANTES de tocar em mais nada.
    if identidade.exists() {
        let guardada = raiz.join(format!("identidade.key.antes-de-restaurar-{carimbo}"));
        std::fs::rename(&identidade, &guardada).map_err(erro)?;
    }
    if indice.exists() {
        let guardado = raiz.join(format!("indice.json.antes-de-restaurar-{carimbo}"));
        std::fs::rename(&indice, &guardado).map_err(erro)?;
    }

    // A semente nova, ATÓMICA e com CHECKSUM — a mesma forma que a criação usa, e não um
    // `fs::write` cru, que deixava a semente restaurada sem soma e sem durabilidade (#7, #18).
    crate::estado::gravar_semente(&identidade, &semente).map_err(erro)?;

    // CONGELAR E REINICIAR (#6, #16).
    //
    // A `App` em memória ainda tem a semente ANTIGA. Sem isto, um par a ligar-se nos segundos
    // seguintes fazia o `guardar_prekey` gravar um índice cifrado com a semente velha ao lado
    // da chave nova — e no arranque seguinte a app não abria. Congela-se para nada se gravar
    // no intervalo, e reinicia-se para não haver intervalo nenhum.
    app.congelar();
    janela.restart();
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

/// Um pedaço de som para cada pessoa da sala — filtrado por quem PERTENCE à sala (#138).
///
/// A interface continua a dizer a quem, porque é ela que sabe quem está na chamada. Mas
/// deixou de ser a única a decidir: a lista passa pelo `participa`, que é a mesma prova que
/// a presença e a sincronização já usavam.
///
/// # A avaria que isto fecha
///
/// A verdade sobre quem recebe o meu microfone vivia SÓ no JavaScript — numa `Map` chamada
/// `voz.presentes`, alimentada por mensagens de presença que chegam da rede. Um par com
/// software modificado que forjasse presença entrava nessa lista, e daí em diante o Rust
/// escrevia-lhe datagramas sem perguntar nada a ninguém. O porteiro existia para a
/// sincronização, para o vídeo e para a presença; a voz — a única coisa que sai desta
/// máquina cinquenta vezes por segundo — passava ao lado dele.
///
/// O filtro é do lado de cá porque é deste lado que estão as provas: o `participa` lê os
/// `peers` da sala, e um `peer` só lá chega tendo escrito uma entrada que DECIFRA com a
/// chave dela.
#[tauri::command]
pub fn enviar_voz(
    servidor: String,
    para: Vec<String>,
    dados: Vec<u8>,
    rede: State<Arc<Rede>>,
    nucleo: State<Arc<crate::estado::App>>,
) {
    let permitidos = crate::rede::so_quem_participa(&nucleo, &servidor, &para);
    if permitidos.is_empty() {
        return;
    }
    rede.enviar_voz(&permitidos, &dados);
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
/// Os últimos acontecimentos de rede, com hora (#119).
///
/// Existe para uma pessoa do outro hemisfério poder copiar isto e colá-lo, em vez de tentar
/// descrever o que viu. E leva chaves e horas — que são metadados —, por isso quem o copia
/// tem de saber o que está a copiar antes de o fazer; é o botão que o diz.
#[tauri::command]
pub fn diario_da_rede(rede: State<Arc<Rede>>) -> Vec<serde_json::Value> {
    let Ok(d) = rede.diario.lock() else {
        return Vec::new();
    };
    d.iter()
        .map(|(h, t)| serde_json::json!({ "hora": h, "texto": t }))
        .collect()
}

/// Por onde é que EU entro na rede (#54).
///
/// O `Endpoint` do iroh expõe isto desde sempre e o Bruma nunca perguntou. A consequência
/// prática: quando a ligação não pega, não havia como distinguir «o meu relay não está a
/// atender» de «ele está offline» de «o furo falhou». Três causas com o mesmo sintoma e
/// nenhuma forma de as separar.
///
/// Mostrar o nome de um servidor do n0 na app pode chocar quem leu «sem servidor» — e é bom
/// que choque: é a verdade, e já está no README.
#[tauri::command]
pub fn entrada_na_rede(rede: State<Arc<Rede>>) -> Vec<serde_json::Value> {
    use iroh::Watcher;
    rede.endpoint
        .home_relay_status()
        .get()
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "url": r.url().to_string(),
                "ligado": r.is_connected(),
                "erro": r.last_error().map(|e| e.to_string()),
            })
        })
        .collect()
}

/// TODAS as ligações abertas — e não só as que a interface se lembrou de perguntar (#48).
///
/// # A avaria que isto fecha
///
/// O painel de rede pedia a qualidade de `voz.presentes`, e o `voz.presentes` só se enche
/// com eventos de `presenca`, que só existem quando alguém está numa sala de VOZ. Fora de
/// uma chamada — que é a maior parte do tempo — o painel escrevia «Ninguém ligado neste
/// momento» com ligações abertas por baixo. Um painel de diagnóstico que só funciona quando
/// não é preciso.
///
/// Cada linha leva a RAZÃO de estar ligada. Sem isso, ver aqui gente com quem não há
/// nenhuma sala aberta neste momento parece uma fuga — e é o contrário: é o vigia a manter
/// de pé exactamente as ligações que os servidores partilhados justificam.
#[tauri::command]
pub fn ligacoes(
    rede: State<Arc<Rede>>,
    nucleo: State<Arc<crate::estado::App>>,
) -> Vec<serde_json::Value> {
    let Ok(l) = rede.ligacoes.lock() else {
        return Vec::new();
    };
    l.keys()
        .map(|p| {
            // Os IDs das salas, e não os nomes: o nome de uma sala vive dentro do log
            // cifrado, não na struct. A interface já os tem resolvidos na `vista`.
            serde_json::json!({ "peer": p, "salas": crate::rede::salas_com(&nucleo, p) })
        })
        .collect()
}

#[tauri::command]
pub fn qualidade(peers: Vec<String>, rede: State<Arc<Rede>>) -> Vec<serde_json::Value> {
    let Ok(ligacoes) = rede.ligacoes.lock() else {
        return Vec::new();
    };
    peers
        .iter()
        .filter_map(|p| {
            let (c, _) = ligacoes.get(p)?;
            // UMA LIGAÇÃO MORTA NÃO DÁ UM RTT ANTIGO (#142). O painel existe para
            // transformar «não se ouve nada» numa resposta, e uma ligação fechada a
            // aparecer com o RTT da última vez que esteve viva é a resposta errada.
            if let Some(razao) = c.close_reason() {
                return Some(serde_json::json!({
                    "peer": p, "caminho": "morta", "relay": false, "ms": null,
                    "morta": razao.to_string(),
                    "enviados": 0, "recebidos": 0, "envS": 0, "recS": 0,
                    "haQuantoRec": null, "vozFalhados": 0, "perda": null,
                    "disseTerEnviado": 0, "filaLivre": 0,
                    "ecraEnviado": 0, "ecraRecebido": 0,
                }));
            }
            let caminhos = c.paths();
            let escolhido = caminhos.iter().find(|x| x.is_selected());
            // ZERO NÃO É «ZERO MILISSEGUNDOS», É «NINGUÉM MEDIU» (#171).
            //
            // O rodapé fazia `pior = max(0, ...ms)` e, se desse zero, escrevia «Voz
            // conectada» a verde. Mas zero é o valor que sai quando não há caminho escolhido
            // ou o RTT ainda não existe — ou seja, o estado «não sei» pintava-se de «óptimo».
            // Agora o RTT é `null` quando não foi medido, e quem lê tem de decidir o que
            // fazer com isso em vez de o confundir com um bom resultado.
            // TRES ESTADOS, E NÃO UM BOOLEANO (#49).
            //
            // Quando não há caminho escolhido, isto devolvia `false` — «não é relay» — e a
            // interface traduzia-o literalmente para «directa». Ou seja: **não sabermos por
            // onde a ligação vai era afirmado como o melhor caso possível**, que é a mesma
            // família de mentira do RTT a zero (#171) e do «Voz conectada» sem voz (#32).
            //
            // O painel vai passar a dizer «caminho desconhecido» com alguma frequência. É
            // desconfortável e é a informação verdadeira.
            let (caminho, ms) = match escolhido {
                Some(x) if x.is_relay() => {
                    ("relay", c.rtt(x.id()).map(|d| d.as_secs_f64() * 1000.0))
                }
                Some(x) => ("directa", c.rtt(x.id()).map(|d| d.as_secs_f64() * 1000.0)),
                None => ("desconhecido", None),
            };
            let agora = std::time::Instant::now();
            let n = rede
                .contagem
                .lock()
                .ok()
                .map(|mut c| {
                    let e = c.entry(p.clone()).or_default();
                    e.recalcular_ritmo(agora);
                    (*e, e.ha_quanto_rec(agora))
                })
                .unwrap_or_default();
            let (n, ha_quanto) = n;
            Some(serde_json::json!({
                "peer": p, "caminho": caminho, "relay": caminho == "relay", "ms": ms,
                "enviados": n.voz_env, "recebidos": n.voz_rec,
                // O que o painel precisa para falar do AGORA e não do acumulado (#32, #33):
                // pacotes por segundo em cada sentido, há quanto tempo chegou o último, e
                // quantos o transporte recusou (#34).
                "envS": n.env_s, "recS": n.rec_s,
                "haQuantoRec": ha_quanto,
                "vozFalhados": n.voz_falhados,
                // O TEMPO DE ESCRITA E O QUE SE CORTOU (#114). Sem estes, o `Lagged` era um
                // acontecimento sem causa visivel.
                "escritaPiorMs": n.escrita_pior_ms,
                "escritaUltimaMs": n.escrita_ultima_ms,
                "videoCortado": n.video_cortado,
                // ESPAÇO LIVRE NA FILA DE DATAGRAMAS (#173). É o que permite saber se os
                // 16 KiB são apertados ou folgados sem adivinhar: se isto nunca se aproximar
                // de zero, a fila nunca esteve perto de encher e há margem para a reduzir.
                "filaLivre": c.datagram_send_buffer_space(),
                // A PERDA (#124): `null` enquanto o outro lado não tiver dito quantos
                // mandou. Ausência de medida não é zero por cento — é a mesma distinção do
                // RTT, e a mesma tentação de a pintar de bom resultado.
                "perda": n.perda_por_cento(),
                "disseTerEnviado": n.disse_ter_enviado,
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
#[cfg(debug_assertions)]
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
#[cfg(debug_assertions)]
#[tauri::command]
pub fn medir_ui_pedido() -> bool {
    std::env::args().any(|a| a == "--medir-ui")
}

#[cfg(test)]
mod testes {
    use super::*;

    /// O carimbo da actualização é lido UMA vez, e só avisa quando deve (#121).
    ///
    /// Os quatro cenários num teste só, de propósito: o ficheiro é um caminho fixo no
    /// `%APPDATA%` — partilhado com a app real e com os outros testes — e um teste por
    /// cenário a correr em paralelo pisava-se a si próprio. Aqui cada cenário escreve,
    /// pergunta, e a própria função apaga.
    #[test]
    fn o_carimbo_da_actualizacao_avisa_quando_deve() {
        let caminho = std::path::PathBuf::from(std::env::var_os("APPDATA").unwrap())
            .join("Bruma")
            .join("actualizacao.json");
        std::fs::create_dir_all(caminho.parent().unwrap()).unwrap();
        let agora = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let escreve = |j: serde_json::Value| std::fs::write(&caminho, j.to_string()).unwrap();

        // Sem carimbo nenhum: nada a dizer. (E deixa o terreno limpo para os seguintes.)
        let _ = std::fs::remove_file(&caminho);
        assert_eq!(actualizacao_incompleta(), None, "sem carimbo não há aviso");

        // «Pronto» é a actualização a acabar bem: silêncio, e o ficheiro desaparece.
        escreve(serde_json::json!({"alvo": "9.9.9", "instante": agora, "estado": "pronto"}));
        assert_eq!(actualizacao_incompleta(), None, "pronto não é aviso");
        assert!(!caminho.exists(), "o carimbo tem de ser apagado ao ler");

        // O alvo é a NOSSA versão: a instalação aconteceu, só o «pronto» se perdeu.
        escreve(serde_json::json!({
            "alvo": env!("CARGO_PKG_VERSION"), "instante": agora, "estado": "a-instalar"
        }));
        assert_eq!(
            actualizacao_incompleta(),
            None,
            "já somos o alvo: não falhou nada"
        );

        // Um carimbo velho de mais ignora-se: um resto esquecido não assusta ninguém.
        escreve(serde_json::json!({
            "alvo": "9.9.9", "instante": agora - 8 * 86_400, "estado": "a-instalar"
        }));
        assert_eq!(
            actualizacao_incompleta(),
            None,
            "mais de uma semana: ignora-se"
        );

        // E o caso real: outra versão, por acabar, recente — AVISA, com as duas versões.
        escreve(serde_json::json!({"alvo": "9.9.9", "instante": agora, "estado": "a-instalar"}));
        let aviso = actualizacao_incompleta().expect("tinha de avisar");
        assert!(aviso.contains("9.9.9"), "o aviso diz o alvo: {aviso}");
        assert!(
            aviso.contains(env!("CARGO_PKG_VERSION")),
            "e diz onde ficámos: {aviso}"
        );
        assert!(
            !caminho.exists(),
            "um aviso, não um eco: o carimbo apaga-se"
        );
        // Segunda leitura: o aviso não se repete.
        assert_eq!(actualizacao_incompleta(), None, "o aviso é um só");
    }

    /// O NOME TEM TECTO — e o tecto conta caracteres, não bytes.
    ///
    /// # A avaria que isto mede
    ///
    /// O `enviar` tinha tecto; o `definir_nome` não tinha nenhum do lado do Rust. Só o
    /// `maxlength="32"` do campo no HTML, que é uma sugestão ao teclado e não um guarda.
    ///
    /// E um nome não é um campo qualquer: o `definir_nome` escreve uma `Carga::Apresentar` no
    /// log de **todos** os servidores onde a pessoa está. Um nome enorme punha em cada uma
    /// dessas salas uma entrada que depois não cabe num quadro de sync — e a partir daí a sala
    /// deixava de sincronizar, por uma coisa que a própria pessoa fez sem querer.
    ///
    /// Contar caracteres e não bytes importa: com bytes, «José» gastava cinco dos trinta e dois
    /// e ninguém percebia porquê.
    #[test]
    fn o_nome_tem_tecto_e_conta_caracteres() {
        assert_eq!(
            nome_aceitavel("  Rakjsu  ").unwrap(),
            "Rakjsu",
            "o nome apara-se"
        );
        assert!(
            nome_aceitavel("   ").is_err(),
            "um nome só de espaços fica vazio"
        );

        // Trinta e dois acentuados: são 64 bytes em UTF-8, e têm de passar.
        let acentuado = "é".repeat(MAX_NOME);
        assert_eq!(
            acentuado.len(),
            MAX_NOME * 2,
            "o caso tem de ser mesmo multibyte"
        );
        assert!(
            nome_aceitavel(&acentuado).is_ok(),
            "o tecto conta caracteres: 32 acentuados têm de caber"
        );

        assert!(
            nome_aceitavel(&"a".repeat(MAX_NOME + 1)).is_err(),
            "um caracter acima do tecto tem de ser recusado"
        );
    }
}
