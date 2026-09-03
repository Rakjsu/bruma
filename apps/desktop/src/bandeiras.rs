//! As bandeiras de ambiente que existem **só para medir**, num sítio só.
//!
//! # Porque é que isto é um módulo e não uma chamada a `env::var` onde é precisa
//!
//! Este projecto força caminhos que não acontecem sozinhos: um codificador que morre a meio,
//! uma captura que falha, uma sessão que cai, um sync lento de propósito. Sem isso, o código
//! que trata dessas falhas nunca corre num teste — e código que nunca corre não está testado,
//! está escrito.
//!
//! Nada disso tem que ver com **usar** a app. Uma pessoa com o `BRUMA_SEM_TRAVAO` posto por
//! engano fica com o travão da partilha de ecrã desligado e nunca saberá porquê: a app não
//! diz nada, só se comporta mal. Um andaime numa instalação a sério é uma armadilha, não uma
//! ferramenta.
//!
//! Estando aqui, cada nome aparece **uma vez**, dentro de um bloco `#[cfg(debug_assertions)]`.
//! Na release a função devolve o valor neutro e o nome nem sequer existe no binário — o que é
//! verificável com uma busca de bytes, e é o que a ferramenta `so-o-que-vai-na-release` faz.
//!
//! `#[cfg]` e não `cfg!()`: com `cfg!()` o comportamento fica certo mas o texto continua no
//! exe e a chamada ao ambiente é feita na mesma. Já medi as duas metades do mesmo padrão a
//! darem resultados diferentes no mesmo compilador.
//!
//! # O que NÃO está aqui
//!
//! `BRUMA_DADOS` (escolher a pasta de dados) e `BRUMA_REGISTO` (nível de registo) são
//! diagnóstico legítimo, estão documentados no README, e servem a quem instalou a app quando
//! alguma coisa corre mal. Ficam onde estão.

/// A sessão morre ENTRE o registo e o stream — o caminho que deixava um par inalcançável.
#[cfg(debug_assertions)]
pub fn sessao_morre() -> bool {
    std::env::var("BRUMA_SESSAO_MORRE").is_ok()
}
#[cfg(not(debug_assertions))]
pub fn sessao_morre() -> bool {
    false
}

/// Ao fim de quantos ms uma sessão JÁ DE PÉ cai, para se poder medir a religação (#56).
///
/// # Porque é que a bandeira de cima não servia
///
/// O `sessao_morre` mata a ligação antes do stream: nunca houve `Ola`, nunca houve presença,
/// nunca houve voz. Prova o caminho da sessão que morre a nascer — que é um caminho real e
/// por isso ela fica — mas **não prova a religação**, que é o que interessa entre os EUA e o
/// Brasil. Depois de uma queda dessas não há nada para recuperar: o outro lado nem sabia que
/// existíamos.
///
/// Esta mata uma sessão inteira, com presença, voz e ecrã a passar. É a única forma de o
/// contador de ligados, a marca «a religar» e o reenvio do cabeçalho deixarem de estar por
/// verificar.
#[cfg(debug_assertions)]
pub fn sessao_morre_ao_fim_de() -> Option<u64> {
    std::env::var("BRUMA_SESSAO_MORRE_MS")
        .ok()
        .and_then(|v| v.parse().ok())
}
#[cfg(not(debug_assertions))]
pub fn sessao_morre_ao_fim_de() -> Option<u64> {
    None
}

/// De quantas em quantas VOLTAS do vigia (2 s cada) se disca a um par já ligado, para forçar a
/// SUBSTITUIÇÃO de sessão (#50).
///
/// # Porque é que a queda forçada não chega
///
/// O `sessao_morre_ao_fim_de` mata a ligação e o vigia religa — mas isso é uma sessão a
/// morrer e outra a nascer, com o mapa vazio pelo meio. O defeito do contador vivia noutro
/// sítio: quando os DOIS lados discam ao mesmo tempo, o desempate faz `Destino::Substitui`,
/// a entrada do mapa passa a ser da sessão nova, e o `Drop` da antiga vê que a série já não
/// é a dela e cala-se. Uma soma sem a subtração correspondente.
///
/// Isso acontece em quase todas as religações reais, e nesta máquina quase nunca — as duas
/// instâncias não estão suficientemente dessincronizadas. Esta bandeira força-o: disca-se a
/// alguém que já está ligado, o outro lado vê uma segunda ligação do mesmo par, e substitui.
#[cfg(debug_assertions)]
pub fn discar_a_dobrar_a_cada_voltas() -> Option<u64> {
    std::env::var("BRUMA_DISCAR_A_DOBRAR")
        .ok()
        .and_then(|v| v.parse().ok())
}
#[cfg(not(debug_assertions))]
pub fn discar_a_dobrar_a_cada_voltas() -> Option<u64> {
    None
}

/// Quantos ms o laço de ESCRITA de cada sessão dorme por volta, para forçar o `Lagged`.
///
/// # Porque é que isto tem de existir
///
/// O canal de difusão tem 512 lugares. Para um receptor se atrasar o suficiente para o
/// `tokio::sync::broadcast` lhe dizer `Lagged` — e deitar fora o que ele não leu — é preciso
/// que o escritor fique para trás de 512 mensagens. Nesta máquina isso não acontece: a
/// escrita é local e instantânea.
///
/// Entre os EUA e o Brasil acontece, e o que se perdia por aí não era só imagem: era TEXTO,
/// em silêncio, sem nada que o fosse buscar. Sem esta bandeira, a correcção do #53 fica a
/// ser uma afirmação sobre um ramo que nada exercita.
#[cfg(debug_assertions)]
pub fn atraso_da_escrita_ms() -> Option<u64> {
    std::env::var("BRUMA_ESCRITA_LENTA_MS")
        .ok()
        .and_then(|v| v.parse().ok())
}

/// Durante quantos segundos o atraso morde. Depois disso a escrita volta ao normal.
///
/// # Porque e que este numero nao e uma escolha de conveniencia
///
/// O canal guarda os ULTIMOS 512 itens. Uma mensagem so se perde se, entre ela ser escrita e
/// o escritor a alcancar, passarem mais 512 por cima dela. Com os ~18 itens por segundo que
/// o par produz, isso sao uns 28 segundos de atraso DEPOIS da ultima mensagem -- e com uma
/// janela mais curta o teste dizia "recuperou" sobre mensagens que nunca chegaram a perder-se.
/// Descobri-o a sabotar: sem a correccao, chegavam na mesma 5 de 5.
#[cfg(debug_assertions)]
pub fn escrita_lenta_ate_s() -> u64 {
    std::env::var("BRUMA_ESCRITA_LENTA_ATE_S")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(45)
}
#[cfg(not(debug_assertions))]
pub fn escrita_lenta_ate_s() -> u64 {
    0
}
#[cfg(not(debug_assertions))]
pub fn atraso_da_escrita_ms() -> Option<u64> {
    None
}

/// O vídeo sai por streams unidireccionais em vez do stream de controlo (#134, passo 2).
///
/// # O interruptor da travessia de duas versões
///
/// A v0.21.0 LÊ os dois caminhos e escreve o antigo. Virar isto antes de as duas máquinas
/// estarem nessa versão mata a partilha de ecrã para quem não actualizou — em silêncio, que
/// é a pior forma. Enquanto isso não estiver confirmado (e a app mostra a versão do outro
/// lado desde a v0.18.0), isto é uma bandeira de teste e mais nada: serve para o `--par`
/// poder provar que o leitor do lado de lá funciona.
///
/// No dia em que virar, deixa de ser bandeira e passa a ser o comportamento.
#[cfg(debug_assertions)]
pub fn video_por_uni() -> bool {
    std::env::var("BRUMA_VIDEO_POR_UNI").is_ok()
}
#[cfg(not(debug_assertions))]
pub fn video_por_uni() -> bool {
    false
}

/// Ao fim de quantos segundos a tratadora de frames finge que o item se fechou (#39).
///
/// O evento `Closed` a sério só o Windows o dispara — quando a janela fecha, o monitor sai,
/// o driver é reposto. Nesta máquina, durante um teste, nada disso acontece. O `Err` daqui
/// segue o MESMO caminho que o `Err` do `on_closed`: o crate guarda-o e devolve-o no
/// `stop()` do vigia. Não é o mesmo evento; é a mesma saída.
#[cfg(debug_assertions)]
pub fn item_fecha_aos() -> Option<u64> {
    std::env::var("BRUMA_ITEM_FECHA_AOS")
        .ok()
        .and_then(|v| v.parse().ok())
}
#[cfg(not(debug_assertions))]
pub fn item_fecha_aos() -> Option<u64> {
    None
}

/// A interface deixa de responder ao `partilha-falhou` com `parar_de_partilhar` (#40).
///
/// Serve para provar que o Rust pára o som SOZINHO quando a imagem morre. Sem isto a prova
/// não discrimina: o salto de ida e volta pela webview também parava o som, e a sabotagem
/// passava.
#[cfg(debug_assertions)]
pub fn ui_surda() -> bool {
    std::env::var("BRUMA_UI_SURDA").is_ok()
}
#[cfg(not(debug_assertions))]
pub fn ui_surda() -> bool {
    false
}

/// Atrasa o sync de propósito, para a janela em que uma mensagem se pode perder ser
/// observável em vez de instantânea.
#[cfg(debug_assertions)]
pub fn sync_lento_ms() -> Option<u64> {
    std::env::var("BRUMA_SYNC_LENTO").ok()?.parse().ok()
}
#[cfg(not(debug_assertions))]
pub fn sync_lento_ms() -> Option<u64> {
    None
}

/// O codificador de ecrã morre ao fim de N pedaços.
#[cfg(debug_assertions)]
pub fn codificador_morre_ao() -> Option<u64> {
    std::env::var("BRUMA_CODIFICADOR_MORRE").ok()?.parse().ok()
}
#[cfg(not(debug_assertions))]
pub fn codificador_morre_ao() -> Option<u64> {
    None
}

/// A captura de ecrã falha à saída da porta.
#[cfg(debug_assertions)]
pub fn falha_captura() -> bool {
    std::env::var("BRUMA_FALHA_CAPTURA").is_ok()
}
#[cfg(not(debug_assertions))]
pub fn falha_captura() -> bool {
    false
}

/// Desliga o travão da partilha de ecrã.
#[cfg(debug_assertions)]
pub fn sem_travao() -> bool {
    std::env::var("BRUMA_SEM_TRAVAO").is_ok()
}
#[cfg(not(debug_assertions))]
pub fn sem_travao() -> bool {
    false
}

/// A captura continua a correr depois de mandada parar, para se medir a fuga.
#[cfg(debug_assertions)]
pub fn so_vigia() -> bool {
    std::env::var("BRUMA_SO_VIGIA").is_ok()
}
#[cfg(not(debug_assertions))]
pub fn so_vigia() -> bool {
    false
}

/// O som morre ao fim de N segundos.
#[cfg(debug_assertions)]
pub fn som_morre_aos() -> Option<u64> {
    std::env::var("BRUMA_SOM_MORRE").ok()?.parse().ok()
}
#[cfg(not(debug_assertions))]
pub fn som_morre_aos() -> Option<u64> {
    None
}

/// Volta ao caminho de eco antigo, para se provar que o novo o corrigiu.
#[cfg(debug_assertions)]
pub fn eco_antigo() -> bool {
    std::env::var("BRUMA_ECO_ANTIGO").is_ok()
}
#[cfg(not(debug_assertions))]
pub fn eco_antigo() -> bool {
    false
}

/// Captura só o som da própria app em vez do sistema todo.
#[cfg(debug_assertions)]
pub fn so_nos() -> bool {
    std::env::var("BRUMA_SO_NOS").is_ok()
}
#[cfg(not(debug_assertions))]
pub fn so_nos() -> bool {
    false
}

/// O codificador nasce com este ritmo INVENTADO, sem esperar pelo que a captura anuncia
/// (#108): é o defeito antigo, a pedido. É a única forma de ver, numa máquina cuja mistura
/// já está a 48 kHz, o que acontecia a quem a tinha a 44,1.
#[cfg(debug_assertions)]
pub fn sondagem_ritmo() -> Option<u32> {
    std::env::var("BRUMA_SONDAGEM_RITMO").ok()?.parse().ok()
}
#[cfg(not(debug_assertions))]
pub fn sondagem_ritmo() -> Option<u32> {
    None
}

/// Quem entra não pede a chave nem faz sair o último frame (#111): é o comportamento
/// antigo, para se medir. Medido nesta máquina: sem diferença — ver `ecra.rs`.
#[cfg(debug_assertions)]
pub fn sem_chave_a_pedido() -> bool {
    std::env::var("BRUMA_SEM_CHAVE_A_PEDIDO").is_ok()
}
#[cfg(not(debug_assertions))]
pub fn sem_chave_a_pedido() -> bool {
    false
}

/// Quantas reaberturas do som são recusadas de propósito (#109). Sem a bandeira, NENHUMA:
/// o valor neutro é zero. A primeira versão devolvia `u64::MAX` como neutro — e com isso a
/// release recusava as cinco reaberturas «a pedido» sem ninguém ter pedido, e o som nunca
/// voltava: a revisão da Fase 7 apanhou-o. Com `5`, as cinco falham e sai «vai sem som».
#[cfg(debug_assertions)]
pub fn som_nao_volta() -> u64 {
    std::env::var("BRUMA_SOM_NAO_VOLTA")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}
#[cfg(not(debug_assertions))]
pub fn som_nao_volta() -> u64 {
    0
}

/// O `moof` segue CRU, sem a tradução para o dialecto do MSE (#43): é o que acontecia a
/// um fragmento que o tradutor recusava, e serve para provar que o espectador se
/// reconstrói quando o buffer recusa um segmento.
#[cfg(debug_assertions)]
pub fn moof_cru() -> bool {
    std::env::var("BRUMA_MOOF_CRU").is_ok()
}
#[cfg(not(debug_assertions))]
pub fn moof_cru() -> bool {
    false
}

/// A captura do som demora estes milissegundos a anunciar o formato (#108). Com mais de
/// seis segundos, o codificador tem de nascer mudo e a thread do som tem de parar sozinha.
#[cfg(debug_assertions)]
pub fn som_demora_ms() -> Option<u64> {
    std::env::var("BRUMA_SOM_DEMORA_MS").ok()?.parse().ok()
}
#[cfg(not(debug_assertions))]
pub fn som_demora_ms() -> Option<u64> {
    None
}

/// O Windows não entrega frame nenhum (#41): `BRUMA_SEM_FRAMES=1` para sempre, ou
/// `BRUMA_SEM_FRAMES_ATE_S=N` só nos primeiros N segundos — para se ver o aviso aparecer
/// E ser retirado quando a imagem volta.
#[cfg(debug_assertions)]
pub fn sem_frames_ate_s() -> Option<u64> {
    if let Ok(n) = std::env::var("BRUMA_SEM_FRAMES_ATE_S") {
        return n.parse().ok();
    }
    std::env::var("BRUMA_SEM_FRAMES")
        .is_ok()
        .then_some(u64::MAX)
}
#[cfg(not(debug_assertions))]
pub fn sem_frames_ate_s() -> Option<u64> {
    None
}

/// O codificador demora estes milissegundos por frame (#41): a fila enche, os frames
/// largam-se, e o vigia tem de o dizer.
#[cfg(debug_assertions)]
pub fn codificador_lento_ms() -> Option<u64> {
    std::env::var("BRUMA_CODIFICADOR_LENTO_MS")
        .ok()?
        .parse()
        .ok()
}
#[cfg(not(debug_assertions))]
pub fn codificador_lento_ms() -> Option<u64> {
    None
}
