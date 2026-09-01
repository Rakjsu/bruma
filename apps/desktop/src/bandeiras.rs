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
