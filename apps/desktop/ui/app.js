/* ==========================================================================
   Bruma — interface.
   Nenhuma chave privada passa por aqui: o JavaScript pede ações, o Rust assina e cifra.
   ========================================================================== */

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

/* ==========================================================================
   Um erro de JavaScript deixa de desaparecer.

   Tudo o que corri hoje para tornar falhas visíveis parou na fronteira do JS: uma excepção
   não apanhada ia para um DevTools que ninguém abre, e o sintoma era a interface deixar de
   responder a meio sem uma palavra em lado nenhum. Foi assim que uma medição minha parou a
   meio e não deu pista nenhuma.

   Agora vai para o mesmo `bruma.log` de tudo o resto.
   ========================================================================== */
(function erros() {
  const contar = (o_que, msg, onde) => {
    try {
      window.__TAURI__.core.invoke('capacidades', {
        linha: `[js] ${o_que}: ${msg}${onde ? ' @ ' + onde : ''}`,
      }).catch(() => {});
    } catch (e) { /* nem o registo há; não há mais nada a fazer */ }
  };
  window.addEventListener('error', ev => {
    contar('erro', ev.message, `${ev.filename || '?'}:${ev.lineno}:${ev.colno}`);
  });
  window.addEventListener('unhandledrejection', ev => {
    const r = ev.reason;
    contar('promessa recusada', (r && (r.stack || r.message)) || String(r), '');
  });
})();

const $ = (s, r = document) => r.querySelector(s);
const $$ = (s, r = document) => [...r.querySelectorAll(s)];

let vista = null;        // o último estado vindo do Rust
// Os amigos, em cache, para o `nomeDoPeer` poder preferir o nome LOCAL sem ser assíncrono.
// A lista nunca sai desta máquina — tê-la aqui não a expõe a nada.
let amigos = [];
// Declarado aqui em cima, e não a meio do ficheiro, porque o `escreverMensagens` e o
// `talvezAvisar` passaram a lê-lo: com `let` declarado depois, uma chamada antes da linha da
// declaração dá ReferenceError em vez de `undefined`, e a app parava sem nada legível.
let janelaComFoco = document.hasFocus();
let servidorAtual = null;
let canalAtual = null;
/** Onde estamos: numa sala de um servidor, ou nas conversas privadas.
 *
 *  O `.rail__mark` do topo da barra deixa de ser um enfeite e passa a ser um destino — é o
 *  mesmo sítio onde o Discord põe a casa. As três colunas da direita são as mesmas; só
 *  muda o que vai dentro delas. */
let modo = 'servidor';
let conversaAtual = null;
let ligados = 0;

/* ---------- identicons: a chave pública desenhada ---------- */

function marcaDaChave(chave) {
  let h = 2166136261;
  for (const c of chave) { h ^= c.charCodeAt(0); h = Math.imul(h, 16777619); }
  let s = h >>> 0;
  const rnd = () => (s = (s * 1664525 + 1013904223) >>> 0) / 4294967296;
  const hue = 150 + Math.floor(rnd() * 130);
  const cor = `hsl(${hue} 42% 62%)`;
  const fundo = `hsl(${hue} 24% 16%)`;
  let celulas = '';
  const rect = (x, y) => `<rect x="${x}" y="${y}" width="1" height="1"/>`;
  for (let y = 0; y < 5; y++) {
    for (let x = 0; x < 3; x++) {
      if (rnd() > 0.48) { celulas += rect(x, y); if (x < 2) celulas += rect(4 - x, y); }
    }
  }
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="-0.4 -0.4 5.8 5.8">` +
    `<rect x="-0.4" y="-0.4" width="5.8" height="5.8" fill="${fundo}"/>` +
    `<g fill="${cor}">${celulas}</g></svg>`;
  return `url("data:image/svg+xml,${encodeURIComponent(svg)}")`;
}

function pintar(el, chave) {
  el.style.backgroundImage = marcaDaChave(chave || 'anon');
  el.style.backgroundSize = 'cover';
}

/* ---------- ajudas de UI ---------- */

const abrir = id => { $('#' + id).hidden = false; };
const fechar = id => { $('#' + id).hidden = true; };

function erroEm(id, msg) {
  const el = $('#' + id);
  el.textContent = msg || '';
}

/** A hora de uma mensagem, no formato que a máquina usa.
 *
 *  Estava escrita à mão em 24 horas. O fuso vinha do Windows, o formato não: quem está
 *  num sítio onde se escreve "3:42 PM" via "15:42". `toLocaleTimeString` sem locale usa o
 *  do sistema, que é precisamente o que as Definições prometem. */
function horaCurta(ms) {
  return new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
}

/** Nunca usar innerHTML com texto de outra pessoa. */
function elemento(tag, cls, texto) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (texto !== undefined) e.textContent = texto;
  return e;
}

function chaveCurta(k) {
  return k ? `${k.slice(0, 4)}·${k.slice(4, 8)}·${k.slice(8, 12)}` : '';
}

/* ---------- desenhar ---------- */

/** Quantas mensagens por ler tem um servidor, somando os canais. */
function porLerNoServidor(s) {
  return Object.values(s.nao_lidos || {}).reduce((a, b) => a + b, 0);
}

/** A bolinha com o número. Acima de 99 diz «99+», como toda a gente faz — o número exacto
 *  deixa de querer dizer nada e passa a estragar o alinhamento. */
function bolha(n) {
  const b = elemento('span', 'bolha', n > 99 ? '99+' : String(n));
  b.title = `${n} por ler`;
  return b;
}

function desenharRail() {
  $('#btn-privado').classList.toggle('is-active', modo === 'privado');
  const rail = $('#rail-servidores');
  rail.textContent = '';
  for (const s of vista.servidores) {
    const b = elemento('button', 'rail__pill', s.nome.slice(0, 2).toUpperCase());
    b.dataset.tip = s.nome;
    if (modo === 'servidor' && s.id === servidorAtual) b.classList.add('is-active');
    b.onclick = () => escolherServidor(s.id);
    const n = porLerNoServidor(s);
    // A bolha vai num invólucro, e não dentro do botão: o `.rail__pill` é redondo e com
    // `overflow` escondido, e a bolha ficava cortada ao meio na borda.
    const caixa = elemento('div', 'rail__slot');
    caixa.append(b);
    if (n) caixa.append(bolha(n));
    rail.append(caixa);
  }
  // E o botão do modo privado: sem isto, uma mensagem privada com o modo servidor à frente
  // não tinha onde aparecer — que é exactamente o caso em que uma notificação serve.
  const priv = $('#btn-privado');
  const antiga = priv.querySelector('.bolha');
  if (antiga) antiga.remove();
  const nc = (vista.conversas || []).reduce((a, c) => a + (c.nao_lidos || 0), 0);
  if (nc) priv.append(bolha(nc));
}

function servidor() {
  return vista.servidores.find(s => s.id === servidorAtual) || null;
}

/** Que conversa mostrar, dado o que estava escolhido e as que existem.
 *
 *  `null` é um estado ESTÁVEL, e não «nenhuma ainda»: quer dizer «estou na vista dos
 *  Amigos». Sem essa distinção, clicar em «Amigos» punha isto a null e o redesenho seguinte
 *  repunha logo a primeira conversa — a vista ficava inalcançável para quem já tivesse
 *  falado com alguém, e com ela o único botão de remover alguém da lista.
 *
 *  Está à parte para se poder provar sem duas máquinas: é uma decisão, não um desenho.
 */
function qualConversa(atual, conversas) {
  if (atual === null) return null;                       // Amigos, de propósito
  if (conversas.some(c => c.id === atual)) return atual; // a escolhida ainda existe
  return conversas[0] ? conversas[0].id : null;          // desapareceu: a primeira, ou Amigos
}

function conversa() {
  return (vista.conversas || []).find(c => c.id === conversaAtual) || null;
}

/** Para onde vai o que se escreve na caixa.
 *
 *  A caixa lia as globais `servidorAtual`/`canalAtual` directamente, e por isso só sabia
 *  escrever num canal de servidor. Aqui pergunta-se ao modo — que é a única coisa que
 *  distingue os dois casos — em vez de duplicar o handler. */
function destinoDeEscrita() {
  if (modo === 'privado') {
    const c = conversa();
    return c ? { servidor: c.id, canal: c.canal } : null;
  }
  return servidorAtual && canalAtual
    ? { servidor: servidorAtual, canal: canalAtual }
    : null;
}

function irParaPrivado() {
  modo = 'privado';
  desenharTudo();
}

function escolherConversa(id) {
  modo = 'privado';
  conversaAtual = id;
  desenharTudo();
}

function desenharConversas() {
  const lista = $('#lista-canais');

  // «Amigos» é o destino por omissão do modo privado, como no Discord. Sem conversa
  // escolhida, é o que se vê.
  const amg = elemento('div', 'chan');
  if (conversaAtual === null) amg.classList.add('is-active');
  amg.append(elemento('span', 'chan__glyph', '☺'));
  amg.append(elemento('span', null, 'Amigos'));
  amg.onclick = () => { conversaAtual = null; desenharTudo(); };
  lista.append(amg);

  const g = elemento('div', 'group');
  g.append(elemento('div', 'group__label', 'Conversas'));
  lista.append(g);

  const cs = vista.conversas || [];
  if (!cs.length) {
    const nada = elemento('div', 'vazio');
    nada.append(elemento('p', null, 'Ainda não tens conversas.'));
    nada.append(elemento('p', 'nota',
      'Abre uma pela lista de membros de um servidor: clique direito numa pessoa, '
      + '"Mensagem privada".'));
    lista.append(nada);
    return;
  }
  for (const c of cs) {
    const l = elemento('div', 'member');
    if (c.id === conversaAtual) l.classList.add('is-active');
    l.dataset.chave = c.com;
    const av = elemento('span', 'ident');
    pintar(av, c.com);
    const txt = elemento('span');
    txt.append(elemento('b', null, c.nome));
    const avisoC = avisoDeVersao(c.com);
    txt.append(elemento('i', avisoC ? 'versao-diferente' : null, avisoC || chaveCurta(c.com)));
    l.append(av, txt);
    if (c.nao_lidos) {
      l.classList.add('tem-novas');
      l.append(bolha(c.nao_lidos));
    }
    l.onclick = () => escolherConversa(c.id);
    // APAGAR UMA CONVERSA (#87). É a única forma de um pedido indesejado ter fim: bloquear a
    // pessoa é um gesto muito maior, e até aqui era o único que havia.
    l.oncontextmenu = ev => {
      ev.preventDefault();
      // E PARA A PROPAGAÇÃO. Sem isto o menu era construído e destruído no mesmo instante:
      // o ouvinte global de `contextmenu` corre a seguir (fase de bolha), a linha da conversa
      // tem a classe `.member` e o `closest('.member')` dele acerta, e o `abrirMenu` começa
      // por limpar o menu inteiro. Nem um piscar se via — só o menu errado, sem o «Apagar
      // esta conversa». A funcionalidade estava morta do lado de quem a usa.
      ev.stopPropagation();
      abrirMenu(ev.clientX, ev.clientY, [
        { rotulo: 'Copiar chave', accao: () => navigator.clipboard.writeText(c.com) },
        '-',
        {
          rotulo: 'Apagar esta conversa',
          perigo: true,
          accao: async () => {
            await invoke('apagar_conversa', { id: c.id }).catch(e => {
              console.warn('não consegui apagar a conversa:', e);
            });
            if (conversaAtual === c.id) conversaAtual = null;
            await desenharTudo();
          },
        },
      ]);
    };
    lista.append(l);
  }
}

function desenharCanais() {
  const lista = $('#lista-canais');
  lista.textContent = '';
  if (modo === 'privado') return desenharConversas();
  const s = servidor();
  if (!s) return;

  const grupos = [
    ['Texto', 'texto'],
    ['Voz', 'voz'],
  ];
  for (const [titulo, tipo] of grupos) {
    const canais = s.canais.filter(c => c.tipo === tipo);
    const g = elemento('div', 'group');
    const cab = elemento('div', 'group__label', titulo);
    const mais = elemento('button', 'porque', '+');
    mais.title = `Criar canal de ${titulo.toLowerCase()}`;
    mais.onclick = ev => { ev.stopPropagation(); abrirNovoCanal(tipo); };
    cab.append(mais);
    g.append(cab);

    for (const c of canais) {
      const b = elemento('button', 'chan');
      b.dataset.canal = c.id;
      if (c.id === canalAtual) b.classList.add('is-active');
      const glifo = elemento('span', 'chan__glyph', tipo === 'voz' ? '♪' : '#');
      b.append(glifo, document.createTextNode(c.nome));
      const x = elemento('button', 'chan__x', '×');
      x.title = 'Apagar canal';
      x.onclick = async ev => {
        ev.stopPropagation();
        await invoke('apagar_canal', { servidor: s.id, canal: c.id }).catch(alertar);
      };
      b.append(x);
      const porLer = (s.nao_lidos || {})[c.id] || 0;
      if (porLer) {
        b.classList.add('tem-novas');
        b.append(bolha(porLer));
      }
      b.onclick = () => escolherCanal(c.id);
      g.append(b);
      if (tipo === 'voz') {
        const dentro = [...voz.presentes.entries()].filter(([, k]) => k === c.id).map(([p]) => p);
        if (voz.canal === c.id) dentro.unshift(voz.eu);
        if (dentro.length) {
          const lista = elemento('div', 'voice-members');
          for (const p of dentro) {
            const linha = elemento('div', 'vm');
            const av = elemento('span', 'ident');
            pintar(av, p);
            linha.append(av, document.createTextNode(p === voz.eu ? 'tu' : nomeDoPeer(p)));
            lista.append(linha);
          }
          g.append(lista);
        }
      }
    }
    lista.append(g);
  }
}

function desenharMembros() {
  const lista = $('#lista-membros');
  lista.textContent = '';
  // Numa conversa não há membros: são duas pessoas, e a outra está no topo.
  $('#bloco-membros').hidden = modo === 'privado';
  if (modo === 'privado') return;
  const s = servidor();
  if (!s) return;
  $('#contagem-membros').textContent =
    s.membros.length === 1 ? '1 membro' : `${s.membros.length} membros`;
  for (const m of s.membros) {
    const linha = elemento('div', 'member');
    linha.dataset.chave = m.chave;
    if (voz.falando.has(m.chave)) linha.classList.add('a-falar');
    const av = elemento('span', 'ident');
    pintar(av, m.chave);
    const bloco = elemento('span');
    bloco.append(elemento('b', null, m.nome));
    const aviso = avisoDeVersao(m.chave);
    bloco.append(elemento(
      'i',
      aviso ? 'versao-diferente' : null,
      // O «fundou este servidor» saiu (#144): era forjável por qualquer membro da sala, e
      // não há substituto honesto. A chave curta é verdade e chega.
      aviso || chaveCurta(m.chave),
    ));
    linha.append(av, bloco);
    lista.append(linha);
  }
}

async function desenharMensagens() {
  if (modo === 'privado') return desenharMensagensPrivadas();
  const stream = $('#stream');
  const s = servidor();
  const canal = s && s.canais.find(c => c.id === canalAtual);

  if (!s) {
    stream.textContent = '';
    const v = elemento('div', 'vazio');
    v.append(elemento('h3', null, 'Ainda não tens servidores'));
    v.append(elemento('p', null,
      'Cria um servidor teu ou entra num com um convite. Não é preciso registar nada em lado nenhum.'));
    const b = elemento('button', 'btn btn--primary', 'Começar');
    b.onclick = () => abrir('veu-novo');
    v.append(b);
    stream.append(v);
    $('#composer').hidden = true;
    return;
  }

  if (!canal) {
    stream.textContent = '';
    const v = elemento('div', 'vazio');
    v.append(elemento('h3', null, 'Escolhe um canal'));
    v.append(elemento('p', null, 'Ou cria um novo com o + ao lado de Texto.'));
    stream.append(v);
    $('#composer').hidden = true;
    return;
  }

  if (canal.tipo === 'voz') {
    stream.textContent = '';
    stream.hidden = true;
    $('#composer').hidden = true;
    desenharVoz();
    return;
  }
  stream.hidden = false;
  $('#vista-voz').hidden = true;
  desenharNaChamada();

  $('#composer').hidden = false;
  $('#entrada').placeholder = `Mensagem para #${canal.nome}`;

  const msgs = await invoke('mensagens', { servidor: s.id, canal: canal.id }).catch(() => []);
  stream.textContent = '';
  if (!msgs.length) {
    const v = elemento('div', 'vazio');
    v.append(elemento('h3', null, `#${canal.nome}`));
    v.append(elemento('p', null, 'Ainda não há nada aqui. Escreve a primeira mensagem.'));
    stream.append(v);
    return;
  }

  await escreverMensagens(stream, msgs, s.id, canal.id);
  // Os avisos do sistema (#196, #131) voltam a seguir ao redesenho que os teria apagado.
  pintarAvisos();
}

/** Escreve a lista de mensagens no stream, com a linha de «novas mensagens» no sítio.
 *
 *  Uma função só, chamada pelas duas vistas, pela mesma razão que o `umaMensagem` existe:
 *  duplicar isto era garantir que um dia divergiam, e a diferença apareceria como «nas
 *  privadas a linha das novas não aparece», sem ninguém saber porquê.
 *
 *  A marcação de lido é feita DEPOIS de desenhar, e com o valor ANTERIOR na mão: se se
 *  marcasse primeiro, a linha aparecia sempre no fim — que é o mesmo que não aparecer.
 */
// Onde ficou a linha de «novas mensagens» do canal que está à frente.
//
// Sem isto, cada `servidor-mudou` voltava a chamar `marcar_lido` e a linha desaparecia: à
// segunda passagem já não havia nada por ler, portanto não havia onde a pôr. Chegava uma
// mensagem e o sítio onde a leitura tinha parado sumia-se — que é precisamente quando ele
// faz falta.
let marcaDaVista = { onde: null, antes: 0 };
// Só para medição: o valor de `marcar` do último pedido feito ao Rust.
let ultimoMarcarPedido = null;

async function escreverMensagens(stream, msgs, servidorId, canalId) {
  const onde = `${servidorId}/${canalId}`;
  if (marcaDaVista.onde !== onde) marcaDaVista = { onde, antes: null };

  // MARCAR COMO LIDO SÓ COM A JANELA À FRENTE.
  //
  // O redesenho corre a cada mensagem que chega, esteja eu a olhar ou não. Marcar sempre
  // fazia a app dar por vista uma mensagem que ninguém viu — e, pior, o aviso do sistema
  // nunca chegava a existir: quando o `talvezAvisar` olhava, a contagem já tinha voltado a
  // zero. O caso em que um aviso serve para alguma coisa era exactamente o caso que ele não
  // cobria.
  // `janelaComFoco` e nao `document.hasFocus()`: a app ja tem a sua nocao de foco, mantida
  // pelos ouvintes de focus/blur e ja usada para decidir se um aviso de voz aparece. Duas
  // fontes de verdade sobre a mesma coisa e a receita para divergirem — e esta e testavel,
  // porque se lhe pode mexer.
  const aFrente = janelaComFoco;
  // O que foi PEDIDO, para a medição poder ver a decisão.
  //
  // Medir o efeito não serve aqui: numa instância só, todas as mensagens são minhas e as
  // minhas nunca contam como por ler — portanto a marca não avança de qualquer maneira, e a
  // medição passava com e sem a correcção. Já lhe chamei uma medição e ela era uma opinião.
  ultimoMarcarPedido = aFrente;
  const antes = await invoke('marcar_lido', {
    servidor: servidorId, canal: canalId, marcar: aFrente,
  }).catch(() => null);

  // A linha fixa-se na PRIMEIRA vez que se olha para este canal, e fica.
  if (marcaDaVista.antes === null && antes !== null) marcaDaVista.antes = antes;
  const corte = marcaDaVista.antes || 0;

  // Estava eu no fim antes de redesenhar? Se estava a ler mais acima, saltar para o fim
  // seria a app a puxar-me a página das mãos de cada vez que alguém escreve.
  const estavaNoFim = stream.scrollHeight - stream.scrollTop - stream.clientHeight < 40;

  stream.textContent = '';
  let anterior = null;
  let linhaPosta = false;
  for (const m of msgs) {
    // A minha própria mensagem nunca puxa a linha: eu sei o que escrevi.
    if (!linhaPosta && corte > 0 && m.ts_ms > corte && m.autor !== vista.chave) {
      stream.append(elemento('div', 'novas-aqui', 'novas mensagens'));
      linhaPosta = true;
      // E a primeira a seguir à linha leva cabeçalho completo, senão fica agarrada por cima
      // dela como se fosse continuação de quem falou antes.
      anterior = null;
    }
    stream.append(umaMensagem(m, anterior));
    anterior = m;
  }
  if (estavaNoFim || marcaDaVista.antes === antes) stream.scrollTop = stream.scrollHeight;

  if (aFrente) {
    // O contador na barra tem de desaparecer agora, e não só no redesenho seguinte. E a
    // fotografia dos avisos também: sem isto ela ficava com a contagem antiga, e a rajada
    // seguinte não parecia uma subida — ficava em silêncio.
    if (porLerAnterior) {
      porLerAnterior.delete(`s:${onde}`);
      porLerAnterior.delete(`c:${servidorId}`);
    }
    await refrescarBolhas();
  }
}

/** Volta a pedir o estado só para as contagens, sem redesenhar a conversa a meio da
 *  leitura — um `desenharTudo()` aqui fazia o stream saltar para o fim. */
async function refrescarBolhas() {
  const novo = await invoke('estado').catch(() => null);
  if (!novo) return;
  vista.servidores = novo.servidores;
  vista.conversas = novo.conversas;
  desenharRail();
  desenharCanais();
}

/** Uma mensagem desenhada, num canal ou numa conversa.
 *
 *  Está à parte porque as duas vistas TÊM de desenhar igual. Duplicar isto era garantir que
 *  um dia divergiam — e a diferença apareceria como "as mensagens privadas estão estranhas",
 *  sem ninguém saber porquê. */
function umaMensagem(m, anterior) {
  const seguida = anterior && anterior.autor === m.autor && m.ts_ms - anterior.ts_ms < 5 * 60_000;
  const art = elemento('article', seguida ? 'msg msg--cont' : 'msg');
  if (!seguida) {
    const av = elemento('span', 'ident ident--lg');
    pintar(av, m.autor);
    art.append(av);
  }
  const corpo = elemento('div', 'msg__body');
  if (!seguida) {
    const cab = elemento('div', 'msg__head');
    cab.append(elemento('b', null, m.autor_nome));
    cab.append(elemento('time', null, horaCurta(m.ts_ms)));
    corpo.append(cab);
  }
  corpo.append(elemento('p', null, m.texto));
  art.append(corpo);
  return art;
}

async function desenharMensagensPrivadas() {
  const stream = $('#stream');
  const c = conversa();
  $('#vista-voz').hidden = true;
  stream.hidden = false;
  stream.textContent = '';

  if (!c) {
    $('#composer').hidden = true;
    await desenharAmigos(stream);
    return;
  }

  $('#composer').hidden = false;
  $('#entrada').placeholder = `Mensagem para ${c.nome}`;
  const msgs = await invoke('mensagens', { servidor: c.id, canal: c.canal }).catch(() => []);
  await escreverMensagens(stream, msgs, c.id, c.canal);
  pintarAvisos();
}

/** A lista de pessoas que EU decidi conhecer.
 *
 *  Não é um estado partilhado: é uma decisão minha, guardada aqui. Alguém pôr-me na lista
 *  dele não me põe na minha — e é por isso que ninguém entra nesta por pedir. */
async function desenharAmigos(stream) {
  const lista = await invoke('amigos').catch(() => amigos);
  amigos = lista;

  const cab = elemento('div', 'vazio');
  cab.style.textAlign = 'left';
  cab.append(elemento('h3', null, 'Amigos'));
  cab.append(elemento('p', 'nota',
    'Não há directório onde te procurarem: quem não tiver a tua chave não te encontra. '
    + 'Para adicionares alguém precisas da chave dele, e ele da tua.'));

  // Adicionar por chave.
  const form = elemento('div', 'caixa__acoes');
  form.style.cssText = 'justify-content:flex-start;flex-wrap:wrap;margin-top:12px';
  const inChave = document.createElement('input');
  inChave.placeholder = 'a chave dele (64 caracteres)';
  inChave.style.cssText = 'flex:1 1 320px;min-width:0';
  const inNome = document.createElement('input');
  inNome.placeholder = 'como lhe queres chamar';
  inNome.style.cssText = 'flex:0 1 180px;min-width:0';
  const bt = elemento('button', 'btn btn--primary', 'Adicionar');
  const nota = elemento('span', 'nota');
  bt.onclick = async () => {
    try {
      await invoke('adicionar_amigo', { chave: inChave.value, nome: inNome.value });
      inChave.value = '';
      inNome.value = '';
      nota.textContent = '';
      await desenharTudo();
    } catch (e) { nota.textContent = String(e); }
  };
  form.append(inChave, inNome, bt, nota);
  cab.append(form);
  stream.append(cab);

  if (!lista.length) {
    const v = elemento('div', 'vazio');
    v.append(elemento('p', null, 'Ainda não tens ninguém na lista.'));
    v.append(elemento('p', 'nota',
      'Podes também adicionar alguém pela lista de membros de um servidor: clique direito, '
      + '"Adicionar aos amigos".'));
    stream.append(v);
    return;
  }

  for (const a of lista) {
    const linha = elemento('div', 'member');
    linha.dataset.chave = a.chave;
    linha.style.cssText = 'margin:2px 8px;align-items:center';
    const av = elemento('span', 'ident');
    pintar(av, a.chave);
    const txt = elemento('span');
    txt.append(elemento('b', null, a.nome));
    txt.append(elemento('i', null,
      chaveCurta(a.chave) + (a.verificado ? ' · chave verificada' : ' · chave por verificar')));
    linha.append(av, txt);

    const acoes = elemento('span', 'caixa__acoes');
    acoes.style.cssText = 'margin:0;gap:6px';
    const falar = elemento('button', 'btn', 'Mensagem');
    falar.onclick = async () => {
      try {
        const id = await invoke('abrir_conversa', { peer: a.chave });
        await desenharTudo();
        escolherConversa(id);
      } catch (e) { alert(String(e)); }
    };
    // A verificação é o que substitui «o servidor garante que este é o João». Sem
    // directório, é a única forma de saber que a chave é de quem julgas.
    const ver = elemento('button', 'btn', a.verificado ? 'Desmarcar' : 'Verifiquei a chave');
    ver.onclick = async () => {
      await invoke('marcar_verificado', { chave: a.chave, verificado: !a.verificado })
        .catch(e => alert(String(e)));
      await desenharTudo();
    };
    const fora = elemento('button', 'btn btn--perigo', 'Remover');
    fora.onclick = async () => {
      await invoke('remover_amigo', { chave: a.chave }).catch(e => alert(String(e)));
      await desenharTudo();
    };
    acoes.append(falar, ver, fora);
    linha.append(acoes);
    stream.append(linha);
  }
}

function desenharTopo() {
  if (modo === 'privado') {
    const c = conversa();
    $('#nome-servidor').textContent = 'Mensagens privadas';
    $('#nome-canal').textContent = c ? c.nome : '—';
    // Uma conversa não é um canal: a arroba diz que do outro lado está uma pessoa, e não
    // uma sala onde qualquer um entra.
    $('#glifo-canal').textContent = '@';
    // Não há convite para uma conversa, e é de propósito: o que a abre são as duas chaves,
    // e não um segredo que se possa reencaminhar a um terceiro.
    $('#btn-convite').style.display = 'none';
    $('#rotulo-peers').textContent = ligados === 1 ? '1 ligado' : `${ligados} ligados`;
    $('#chip-peers').querySelector('.dot').className = ligados > 0 ? 'dot dot--ok' : 'dot';
    return;
  }
  const s = servidor();
  const canal = s && s.canais.find(c => c.id === canalAtual);
  $('#nome-servidor').textContent = s ? s.nome : '—';
  $('#nome-canal').textContent = canal ? canal.nome : '—';
  $('#glifo-canal').textContent = canal && canal.tipo === 'voz' ? '♪' : '#';
  $('#btn-convite').style.display = s ? '' : 'none';
  $('#rotulo-peers').textContent = ligados === 1 ? '1 ligado' : `${ligados} ligados`;
  $('#chip-peers').querySelector('.dot').className = ligados > 0 ? 'dot dot--ok' : 'dot';
}

async function desenharTudo() {
  vista = await invoke('estado');
  amigos = await invoke('amigos').catch(() => amigos);
  $('#meu-nome').textContent = vista.nome || 'sem nome';
  $('#minha-chave').textContent = chaveCurta(vista.chave);
  pintar($('#meu-avatar'), vista.chave);

  // A auto-selecção corre SÓ no modo servidor. No modo privado `servidorAtual` é nulo de
  // propósito, e isto atirava-nos de volta para um servidor a cada `servidor-mudou` — que
  // acontece a cada mensagem que chega.
  if (modo === 'servidor') {
    if (!vista.servidores.some(s => s.id === servidorAtual)) {
      servidorAtual = vista.servidores[0] ? vista.servidores[0].id : null;
      canalAtual = null;
    }
    const s = servidor();
    if (s && !s.canais.some(c => c.id === canalAtual)) {
      const primeiro = s.canais.find(c => c.tipo === 'texto') || s.canais[0];
      canalAtual = primeiro ? primeiro.id : null;
    }
  } else {
    conversaAtual = qualConversa(conversaAtual, vista.conversas || []);
  }

  desenharRail();
  desenharCanais();
  desenharMembros();
  desenharTopo();
  await desenharMensagens();
  desenharRodape();
}

function escolherServidor(id) {
  modo = 'servidor';
  servidorAtual = id;
  canalAtual = null;
  desenharTudo();
}

function escolherCanal(id) {
  canalAtual = id;
  desenharCanais();
  desenharTopo();
  desenharMensagens();
}

function alertar(e) {
  console.error(e);
  erroEm('erro-novo', String(e));
}

/* ---------- ações ---------- */

$('#btn-novo').onclick = () => { erroEm('erro-novo', ''); abrir('veu-novo'); };
$('#fechar-novo').onclick = () => fechar('veu-novo');

$('#ok-servidor').onclick = async () => {
  const nome = $('#in-servidor').value.trim();
  if (!nome) return erroEm('erro-novo', 'dá um nome ao servidor');
  try {
    const id = await invoke('criar_servidor', { nome });
    $('#in-servidor').value = '';
    fechar('veu-novo');
    servidorAtual = id; canalAtual = null;
    await desenharTudo();
  } catch (e) { erroEm('erro-novo', String(e)); }
};

$('#ok-convite').onclick = async () => {
  const codigo = $('#in-convite').value.trim();
  if (!codigo) return erroEm('erro-novo', 'cola o código do convite');
  erroEm('erro-novo', 'a ligar ao anfitrião…');
  try {
    const id = await invoke('entrar_com_convite', { codigo });
    $('#in-convite').value = '';
    fechar('veu-novo');
    servidorAtual = id; canalAtual = null;
    await desenharTudo();
  } catch (e) { erroEm('erro-novo', String(e)); }
};

function abrirNovoCanal(tipo) {
  $('#in-canal').value = '';
  $('#in-tipo').value = tipo || 'texto';
  erroEm('erro-canal', '');
  abrir('veu-canal');
  $('#in-canal').focus();
}
$('#fechar-canal').onclick = () => fechar('veu-canal');
$('#ok-canal').onclick = async () => {
  const nome = $('#in-canal').value.trim();
  if (!nome) return erroEm('erro-canal', 'dá um nome ao canal');
  try {
    await invoke('criar_canal', { servidor: servidorAtual, nome, tipo: $('#in-tipo').value });
    fechar('veu-canal');
    await desenharTudo();
  } catch (e) { erroEm('erro-canal', String(e)); }
};

$('#btn-convite').onclick = async () => {
  try {
    const codigo = await invoke('criar_convite', { servidor: servidorAtual });
    $('#out-convite').value = codigo;
    $('#copiado').textContent = '';
    abrir('veu-convite');
  } catch (e) { console.error(e); }
};
$('#fechar-convite').onclick = () => fechar('veu-convite');
$('#copiar-convite').onclick = async () => {
  await navigator.clipboard.writeText($('#out-convite').value);
  $('#copiado').textContent = 'copiado';
};

$('#btn-perfil').onclick = () => abrirDefinicoes();
$('#btn-privado').onclick = () => irParaPrivado();

/* ==========================================================================
   Definições, em ecrã inteiro.

   # A regra que manda aqui

   Cinco das secções não têm nada por trás — e é isso que elas dizem. A tentação
   era enchê-las com interruptores plausíveis, e seria a pior coisa possível: um
   interruptor que não faz nada é uma mentira que a pessoa só descobre quando
   precisava dele. Ficam listadas, a cinzento, a explicar o que falta e porquê.

   Uma delas nunca vai ter nada, e isso é uma característica: não há cobranças
   porque não há servidor para pagar nem subscrição para vender.
   ========================================================================== */

const SEM_MOVIMENTO = 'bruma.sem-movimento';
const SEM_JOGO = 'bruma.sem-deteccao-de-jogo';

/** Se a deteção de jogo está desligada. Lê-se em `desenharJogo`. */
function deteccaoDeJogoDesligada() {
  return localStorage.getItem(SEM_JOGO) === '1';
}

function aplicarMovimento() {
  document.documentElement.classList.toggle(
    'sem-movimento', localStorage.getItem(SEM_MOVIMENTO) === '1');
}
aplicarMovimento();

/** O que o último teste do microfone disse. Sobrevive a redesenhos do painel (#107). */
let testeDoMicro = null;
let testeACorrer = false;

/** Prova o microfone SEM precisar do amigo online (#107).
 *
 *  # Porque é que isto tinha de existir
 *
 *  O `--par` prova a metade do meio, mas exige duas instâncias e uma linha de comandos. O
 *  autoteste de voz já fazia exactamente o circuito certo — microfone → Opus → descodificador
 *  — mas corria só sob `--autoteste` e escrevia para a consola do Rust, que a pessoa que
 *  carrega no botão do microfone nunca vê.
 *
 *  Prova TRÊS coisas de uma vez, e nomeia qual das três falhou: o dispositivo capta, o codec
 *  desta máquina existe e funciona, e a saída toca.
 *
 *  # Grava primeiro, toca depois
 *
 *  Nunca ao mesmo tempo. Tocar a própria voz pelas colunas com o microfone aberto é
 *  realimentação garantida — o teste faria barulho em vez de dizer alguma coisa.
 */
/** Devolve `true` se chegou a correr — incluindo quando o veredicto é «está mau», porque
 *  nomear a metade que falhou É o trabalho — e `false` só quando se RECUSOU a correr.
 *
 *  Uma função que se pode recusar a fazer o trabalho tem de o dizer a quem a chamou — e é
 *  também a única forma de a medição provar a guarda de reentrância em vez de a afirmar.
 */
async function testarMicrofone() {
  if (testeACorrer) return false;
  // NÃO DURANTE UMA CHAMADA, e o motivo não é cautela — é que o teste ficaria a mentir das
  // duas pontas ao mesmo tempo.
  //
  // O teste fecha a SUA captura antes de tocar, e é isso que impede a realimentação dele
  // próprio. Mas numa chamada há um SEGUNDO microfone aberto — o `voz.micro` — e esse
  // continua a captar: a gravação sairia pelas colunas e entrava por ele, ou seja o outro
  // lado ouvia a minha voz repetida sem perceber porquê. E a gravação apanharia a voz DELE
  // vinda das colunas, portanto o «captou som» deixava de ser sobre o meu microfone.
  //
  // E numa chamada este teste não é preciso: há alguém do outro lado, que é a prova.
  if (voz.canal) {
    testeDoMicro = {
      estado: 'mau',
      texto: 'Este teste toca-te a tua própria voz de volta, e com uma chamada aberta isso '
        + 'entrava outra vez pelo teu microfone — o outro lado ouvia-se a ele próprio. Sai '
        + 'da chamada para testar. (Numa chamada já tens quem te diga se te ouve.)',
    };
    await mostrarPainel('voz');
    return false;
  }
  testeACorrer = true;
  testeDoMicro = { estado: 'a gravar', texto: 'A gravar 3 segundos… fala agora.' };
  await mostrarPainel('voz');

  let mic = null;
  const falhou = (onde, porque) => {
    testeDoMicro = { estado: 'mau', texto: `${onde}: ${porque}` };
  };
  try {
    if (typeof MediaStreamTrackProcessor === 'undefined') {
      falhou('O codec desta máquina', 'esta versão do WebView2 não sabe entregar som ao '
        + 'codificador. Actualizar o Edge WebView2 resolve.');
      return true;
    }
    try {
      mic = await abrirMicrofone();
    } catch (e) {
      falhou('O dispositivo', `não abriu (${e && e.message ? e.message : e}).`);
      return true;
    }
    const faixa = mic.getAudioTracks()[0];
    if (!faixa) { falhou('O dispositivo', 'abriu e não deu nenhuma faixa de som.'); return true; }

    // O MESMO codificador e descodificador do caminho a sério, com a mesma configuração:
    // um teste com outra configuração provaria outra coisa.
    const pedacos = [];
    let codificados = 0, erroDoCodec = null;
    const dec = new AudioDecoder({
      output: som => {
        const f = new Float32Array(som.numberOfFrames);
        try { som.copyTo(f, { planeIndex: 0, format: 'f32-planar' }); pedacos.push(f); }
        catch (e) { /* formato inesperado */ }
        som.close();
      },
      error: e => { erroDoCodec = e; },
    });
    dec.configure({ codec: 'opus', sampleRate: VOZ_HZ, numberOfChannels: 1 });
    const enc = new AudioEncoder({
      output: p => {
        codificados += 1;
        const b = new Uint8Array(p.byteLength);
        p.copyTo(b);
        try {
          dec.decode(new EncodedAudioChunk({ type: 'key', timestamp: p.timestamp, data: b }));
        } catch (e) { /* segue */ }
      },
      error: e => { erroDoCodec = e; },
    });
    enc.configure({
      codec: 'opus', sampleRate: VOZ_HZ, numberOfChannels: 1,
      bitrate: VOZ_BITRATE, opus: { frameDuration: VOZ_QUADRO_US },
    });

    const leitor = new MediaStreamTrackProcessor({ track: faixa }).readable.getReader();
    const fim = performance.now() + 3000;
    let cruas = 0, energiaCrua = 0;
    while (performance.now() < fim) {
      const { value, done } = await leitor.read().catch(() => ({ done: true }));
      if (done) break;
      // A energia mede-se ANTES do codec: é isso que separa «não captaste nada» de
      // «captaste e o codec comeu».
      try {
        const f = new Float32Array(value.numberOfFrames);
        value.copyTo(f, { planeIndex: 0, format: 'f32-planar' });
        for (let i = 0; i < f.length; i++) energiaCrua += f[i] * f[i];
        cruas += f.length;
      } catch (e) { /* formato inesperado */ }
      // COM GUARDA. Um `AudioEncoder` que entra em erro passa a `closed`, e `encode()`
      // sobre ele ATIRA — a excepção saltava daqui para o `catch` do bloco todo e o
      // veredicto saía «O teste: ...» em vez de «O codec desta máquina: ...», que é
      // precisamente a metade que o teste existe para nomear.
      if (enc.state === 'configured') {
        try { enc.encode(value); } catch (e) { erroDoCodec = erroDoCodec || e; }
      }
      value.close();
    }
    await enc.flush().catch(() => {});
    await dec.flush().catch(() => {});
    try { leitor.cancel(); } catch (e) { /* já */ }
    // FECHA-SE A CAPTURA ANTES DE TOCAR. Esta linha é o teste todo: com ela aberta, o que
    // sai das colunas volta a entrar pelo microfone.
    mic.getTracks().forEach(t => t.stop());
    mic = null;
    try { enc.close(); } catch (e) { /* já */ }
    try { dec.close(); } catch (e) { /* já */ }

    const rms = cruas ? Math.sqrt(energiaCrua / cruas) : 0;
    if (rms < CHAO_DO_MICRO) {
      falhou('O teu microfone', `abriu mas não captou nada (nível ${rms.toFixed(4)}). Está `
        + 'silenciado no Windows, tem o botão físico desligado, ou é o dispositivo errado.');
      return true;
    }
    if (erroDoCodec || !codificados || !pedacos.length) {
      falhou('O codec desta máquina', `captou som (nível ${rms.toFixed(3)}) mas o Opus não `
        + `fechou o circuito: ${codificados} pedaços codificados, ${pedacos.length} `
        + `descodificados${erroDoCodec ? ` (${erroDoCodec.message || erroDoCodec})` : ''}.`);
      return true;
    }

    // E toca-se o que passou pelo codec — não o que entrou. É a única forma de a pessoa
    // ouvir exactamente o que o outro lado ouviria.
    const total = pedacos.reduce((a, f) => a + f.length, 0);
    const ctx = contextoDeAudio();
    const buf = ctx.createBuffer(1, total, VOZ_HZ);
    const canal = buf.getChannelData(0);
    let i = 0;
    for (const f of pedacos) { canal.set(f, i); i += f.length; }
    const fonte = ctx.createBufferSource();
    fonte.buffer = buf;
    fonte.connect(ctx.destination);
    testeDoMicro = {
      estado: 'bom',
      texto: `Captou (nível ${rms.toFixed(3)}), o Opus fechou o circuito `
        + `(${codificados} pedaços) e está a tocar-te de volta ${(total / VOZ_HZ).toFixed(1)} s. `
        + 'Se ouvires a tua voz, as três metades estão boas.',
    };
    await mostrarPainel('voz');
    fonte.start();
    fonte.onended = async () => {
      if (testeDoMicro && testeDoMicro.estado === 'bom') {
        testeDoMicro = { estado: 'bom', texto: testeDoMicro.texto.replace('está a tocar-te',
          'tocou-te') };
        if (aVerAsDefinicoesDaVoz()) await mostrarPainel('voz');
      }
    };
  } catch (e) {
    falhou('O teste', String(e && e.message ? e.message : e));
  } finally {
    if (mic) mic.getTracks().forEach(t => t.stop());
    testeACorrer = false;
    if (aVerAsDefinicoesDaVoz()) await mostrarPainel('voz');
  }
  return true;
}

/** Um interruptor com título e explicação, como os do Discord. */
function interruptor(titulo, explica, ligado, aoMudar) {
  const l = elemento('label', 'def__linha');
  const c = document.createElement('input');
  c.type = 'checkbox';
  c.checked = !!ligado;
  c.onchange = () => aoMudar(c.checked);
  l.append(c);
  const t = elemento('span');
  t.append(elemento('b', null, titulo));
  t.append(elemento('i', null, explica));
  l.append(t);
  return l;
}

function seccao(titulo) {
  const d = elemento('div', 'def__sec');
  d.append(elemento('div', 'members__label', titulo));
  return d;
}

/** O que dizer quando não há nada. Honesto, e com o que falta para haver. */
function aindaNaoHa(oQue, porque) {
  const d = elemento('div', 'def__sec');
  d.append(elemento('div', 'members__label', 'Ainda não existe'));
  const a = elemento('div', 'aviso');
  a.append(elemento('b', null, oQue));
  a.append(document.createTextNode(' ' + porque));
  d.append(a);
  return d;
}

const PAINEIS = {
  conta: {
    nome: 'Conta',
    grupo: 'Definições do utilizador',
    ico: '<circle cx="8" cy="5.6" r="2.8"/><path d="M2.8 14c0-2.9 2.4-4.6 5.2-4.6s5.2 1.7 5.2 4.6"/>',
    desenha: async painel => {
      painel.append(elemento('h2', null, 'Conta'));
      painel.append(elemento('p', null,
        'Não há conta, e-mail nem palavra-passe. A tua identidade é uma chave que foi '
        + 'criada neste computador — ninguém a registou em lado nenhum.'));

      const s1 = seccao('Como apareces');
      const inp = document.createElement('input');
      inp.id = 'def-nome';
      inp.maxLength = 32;
      inp.placeholder = 'o teu nome';
      inp.value = vista.nome || '';
      s1.append(inp);
      const erro = elemento('div', 'caixa__erro');
      erro.id = 'def-erro-nome';
      s1.append(erro);
      const acs = elemento('div', 'caixa__acoes');
      acs.style.justifyContent = 'flex-start';
      const guardar = elemento('button', 'btn btn--primary', 'Guardar');
      const nota = elemento('span', 'nota');
      guardar.onclick = async () => {
        const nome = inp.value.trim();
        if (!nome) { erro.textContent = 'escreve um nome'; return; }
        try {
          await invoke('definir_nome', { nome });
          erro.textContent = '';
          nota.textContent = 'guardado';
          await desenharTudo();
          $('#defs-nome').textContent = nome;
        } catch (e) { erro.textContent = String(e); }
      };
      acs.append(guardar, nota);
      s1.append(acs);
      painel.append(s1);

      const s2 = seccao('A tua chave pública');
      s2.append(elemento('p', 'nota', 'É o teu ID — é por ela que os outros te reconhecem.'));
      const chave = elemento('div', 'def__chave',
        (await invoke('meu_endereco').catch(() => '')) || '—');
      s2.append(chave);
      const a2 = elemento('div', 'caixa__acoes');
      a2.style.justifyContent = 'flex-start';
      const cp = elemento('button', 'btn', 'Copiar a chave');
      cp.onclick = () => navigator.clipboard.writeText(chave.textContent.trim())
        .then(() => { cp.textContent = 'copiada'; setTimeout(() => { cp.textContent = 'Copiar a chave'; }, 1400); })
        .catch(() => {});
      a2.append(cp);
      s2.append(a2);
      painel.append(s2);

      painel.append(seccaoDasPalavras());
    },
  },

  dados: {
    nome: 'Dados e privacidade',
    grupo: 'Definições do utilizador',
    ico: '<path d="M8 1.8 2.8 4v4c0 3.2 2.2 5.6 5.2 6.4 3-.8 5.2-3.2 5.2-6.4V4Z"/>',
    desenha: async painel => {
      painel.append(elemento('h2', null, 'Dados e privacidade'));
      painel.append(elemento('p', null,
        'Não há servidor. Tudo o que escreves vive nesta máquina e nas máquinas de quem '
        + 'está contigo — não há um sítio central onde o histórico se acumule.'));

      const sobre = await invoke('sobre_esta_instalacao').catch(() => null);
      const s1 = seccao('Onde ficam as tuas coisas');
      if (sobre) {
        s1.append(elemento('div', 'def__valor', sobre.pasta));
        s1.append(elemento('p', 'nota',
          'Fora da pasta do programa, para sobreviverem a actualizações e desinstalações.'));
      }
      const a1 = elemento('div', 'caixa__acoes');
      a1.style.justifyContent = 'flex-start';
      const abrir = elemento('button', 'btn', 'Abrir a pasta');
      const nota1 = elemento('span', 'nota');
      abrir.onclick = () => invoke('abrir_pasta_de_dados')
        .catch(e => { nota1.textContent = `não consegui abrir: ${e}`; });
      a1.append(abrir, nota1);
      s1.append(a1);
      painel.append(s1);

      const s2 = seccao('O que a cifra protege — e o que não protege');
      const lista = elemento('div', 'aviso');
      lista.innerHTML =
        '<b>Protege:</b> o que sai desta máquina, e o histórico em disco.<br>'
        + '<b>Não protege:</b> quem já tem a tua <code>identidade.key</code> — para todos os '
        + 'efeitos, essa pessoa é tu.<br>'
        + '<b>Não esconde:</b> quem fala com quem e quando. A isso chama-se metadados.';
      s2.append(lista);
      painel.append(s2);

      const s3 = seccao('Deteção de jogo');
      s3.append(interruptor(
        'Não olhar para as minhas janelas',
        'O rodapé mostra o jogo que tens aberto para o poderes partilhar num clique. '
        + 'Para isso lê os títulos das janelas — nesta máquina, e nunca sai daqui.',
        deteccaoDeJogoDesligada(),
        v => { localStorage.setItem(SEM_JOGO, v ? '1' : '0'); verJogo(); },
      ));
      painel.append(s3);
    },
  },

  permissoes: {
    nome: 'Permissões de mensagens',
    grupo: 'Definições do utilizador',
    ico: '<path d="M3 3.4h10v7.2H6.4L3.4 13Z"/>',
    desenha: async painel => {
      painel.append(elemento('h2', null, 'Permissões de mensagens'));
      painel.append(elemento('p', null,
        'Não há directório: ninguém te encontra sem já ter a tua chave. Isto é sobre o que '
        + 'acontece a quem já a tem.'));

      const p = await invoke('permissoes')
        .catch(() => ({ bloqueados: [], quem_escreve: 'todos' }));

      const s1 = seccao('Quem me pode abrir uma conversa');
      const opcoes = [
        ['todos', 'Toda a gente que tenha a minha chave',
          'Como sempre foi. Sem directório, isso já é um grupo pequeno: quem não a tem não '
          + 'te alcança.'],
        ['salas', 'Só quem partilha uma sala comigo, e os amigos',
          'É o critério que o Discord usa — e aqui é exacto, porque partilhar uma sala '
          + 'prova-se com a chave dela, e não com uma lista que alguém mantém.'],
        ['amigos', 'Só quem eu pus na minha lista',
          'O mais fechado. Uma pessoa nova tem de entrar primeiro na lista de amigos.'],
      ];
      for (const [valor, titulo, explica] of opcoes) {
        const l = elemento('label', 'def__linha');
        const r = document.createElement('input');
        r.type = 'radio';
        r.name = 'quem-escreve';
        r.value = valor;
        r.checked = p.quem_escreve === valor;
        r.onchange = async () => {
          await invoke('definir_quem_escreve', { politica: valor })
            .catch(e => alert(String(e)));
          await mostrarPainel('permissoes');
        };
        l.append(r);
        const t = elemento('span');
        t.append(elemento('b', null, titulo));
        t.append(elemento('i', null, explica));
        l.append(t);
        s1.append(l);
      }
      s1.append(elemento('p', 'nota',
        'Isto decide quem consegue COMEÇAR uma conversa contigo. Quem já tem uma aberta '
        + 'continua a poder escrever nela — fechar essa porta é bloquear.'));
      painel.append(s1);

      const s2 = seccao('Bloqueados');
      s2.append(elemento('p', 'nota',
        'O bloqueio é LOCAL: deixas de aceitar o que ele manda, não o impedes de tentar. '
        + 'Não há servidor no meio para o impedir por ti. Em compensação, ele não distingue '
        + 'estar bloqueado de tu estares desligado — a ligação fecha-se sem uma palavra.'));

      const linha = elemento('div', 'caixa__acoes');
      linha.style.cssText = 'justify-content:flex-start;flex-wrap:wrap';
      const inB = document.createElement('input');
      inB.placeholder = 'a chave de quem queres recusar';
      inB.style.cssText = 'flex:1 1 320px;min-width:0';
      const btB = elemento('button', 'btn btn--perigo', 'Bloquear');
      const notaB = elemento('span', 'nota');
      btB.onclick = async () => {
        try {
          await invoke('bloquear', { chave: inB.value, sim: true });
          inB.value = '';
          // A APP INTEIRA, e nao so este painel.
          //
          // Bloquear tira a pessoa dos amigos e fecha-lhe a ligacao -- e a lista de amigos e
          // a grelha da chamada continuavam a mostra-la, porque nada aqui chamava o
          // `desenharTudo`. Fechavam-se as Definicoes e la estava ela, como amiga e como
          // presente. A app dizia duas coisas contrarias sobre a mesma pessoa.
          await desenharTudo();
          await mostrarPainel('permissoes');
        } catch (e) { notaB.textContent = String(e); }
      };
      linha.append(inB, btB, notaB);
      s2.append(linha);

      if (!p.bloqueados.length) {
        s2.append(elemento('p', 'nota', 'Não bloqueaste ninguém.'));
      } else {
        for (const c of p.bloqueados) {
          const l = elemento('div', 'member');
          l.style.cssText = 'margin:4px 0;align-items:center';
          const av = elemento('span', 'ident');
          pintar(av, c);
          const t = elemento('span');
          t.append(elemento('b', null, nomeDoPeer(c)));
          t.append(elemento('i', null, chaveCurta(c)));
          const bt = elemento('button', 'btn', 'Desbloquear');
          bt.onclick = async () => {
            await invoke('bloquear', { chave: c, sim: false }).catch(e => alert(String(e)));
            await desenharTudo();
            await mostrarPainel('permissoes');
          };
          l.append(av, t, bt);
          s2.append(l);
        }
        s2.append(elemento('p', 'nota',
          'Bloquear alguém tira-o também da lista de amigos: tê-lo nas duas seria a app a '
          + 'dizer duas coisas contrárias sobre a mesma pessoa.'));
      }
      painel.append(s2);

      const s3 = seccao('Ter a certeza de com quem falas');
      s3.append(elemento('p', 'nota',
        'Ninguém garante que uma chave é de quem julgas — não há servidor a dizer «este é o '
        + 'João». Compara a chave com a pessoa por outro caminho, e marca-a como verificada '
        + 'na lista de amigos.'));
      const a3 = elemento('div', 'caixa__acoes');
      a3.style.justifyContent = 'flex-start';
      const ir = elemento('button', 'btn', 'Ver a lista de amigos');
      ir.onclick = () => {
        fecharDefinicoes();
        conversaAtual = null;
        irParaPrivado();
      };
      a3.append(ir);
      s3.append(a3);
      painel.append(s3);

      const s4 = seccao('O que não existe aqui');
      const nao = elemento('div', 'aviso');
      nao.append(elemento('p', null,
        'Filtros de conteúdo sensível e de spam: não há servidor a analisar nada — e não há '
        + 'imagens. O que os substitui é a definição em cima: quem não te pode escrever, não '
        + 'te escreve.'));
      nao.append(elemento('p', null,
        'Expulsar de um servidor: o convite carrega a chave que o decifra e nunca expira. '
        + 'Enquanto essa chave não puder rodar, qualquer expulsão seria teatro — o expulso '
        + 'continuaria a decifrar tudo o que fosse escrito a seguir.'));
      nao.append(elemento('p', null,
        'Amigos de amigos: obrigaria a mostrar a tua lista de amigos aos teus amigos. Ela '
        + 'nunca sai desta máquina.'));
      s4.append(nao);
      painel.append(s4);

      const s5 = seccao('O que uma conversa não esconde');
      const custo = elemento('div', 'aviso');
      custo.append(elemento('p', null,
        'O teu IP, a quem se ligar directamente a ti — que é o caso normal, e o mais '
        + 'rápido. Por relay não vê.'));
      custo.append(elemento('p', null,
        'Que falas com alguém, e quando: o relay vê que chaves falam entre si. Nunca vê o '
        + 'conteúdo.'));
      custo.append(elemento('p', null,
        'O passado, a quem obtiver a tua semente: a chave de uma conversa sai de um '
        + 'Diffie-Hellman entre duas chaves fixas, e não roda.'));
      s5.append(custo);
      painel.append(s5);
    },
  },
  notificacoes: {
    nome: 'Notificações',
    grupo: 'Definições do utilizador',
    ico: '<path d="M4.4 6.6a3.6 3.6 0 0 1 7.2 0c0 3 1.2 4 1.2 4H3.2s1.2-1 1.2-4Z"/><path d="M6.6 13a1.6 1.6 0 0 0 2.8 0"/>',
    desenha: async painel => {
      painel.append(elemento('h2', null, 'Notificações'));
      painel.append(elemento('p', null,
        'Aqui não há servidor a guardar mensagens para te avisar depois: o aviso nasce nesta '
        + 'máquina, quando a mensagem chega.'));

      const s1 = seccao('Avisos do sistema');
      s1.append(interruptor(
        'Avisar-me quando chegar uma mensagem',
        'Só com a janela do Bruma fora da frente. Se estiveres a olhar para ela, já a viste.',
        avisosLigados(),
        v => { localStorage.setItem(AVISOS, v ? '1' : '0'); mostrarPainel('notificacoes'); },
      ));
      s1.append(interruptor(
        'Mostrar também o texto da mensagem',
        'Desligado de propósito. Um aviso do Windows não é a app: aparece no ecrã bloqueado, '
        + 'fica no histórico de notificações do sistema e é lido por quem passar ao pé do '
        + 'computador. O Bruma existe para o conteúdo não sair cifrado de ponta a ponta — '
        + 'copiá-lo para ali desfaz isso, e nada do que se faça aqui o pode desfazer de '
        + 'volta. Sem isto ligado, o aviso diz quem e onde, nunca o quê.',
        avisosComTexto(),
        v => { localStorage.setItem(AVISOS_TEXTO, v ? '1' : '0'); mostrarPainel('notificacoes'); },
      ));

      const acoes = elemento('div', 'caixa__acoes');
      acoes.style.justifyContent = 'flex-start';
      const testar = elemento('button', 'btn', 'Experimentar um aviso');
      const nota = elemento('span', 'nota');
      testar.onclick = async () => {
        nota.textContent = 'a pedir…';
        const foi = await avisar('Bruma', 'É assim que um aviso aparece.');
        nota.textContent = foi
          ? 'apareceu — se não viste, o Windows pode ter os avisos desligados para esta app'
          : 'não apareceu: ou os avisos estão desligados aqui em cima, ou o Windows recusou '
            + 'a permissão';
      };
      acoes.append(testar, nota);
      s1.append(acoes);
      painel.append(s1);

      const s2 = seccao('Por ler');
      s2.append(elemento('p', 'nota',
        'A contagem por canal e por conversa está sempre ligada, e vive só nesta máquina — '
        + 'guardada dentro do índice, que é cifrado. Saber que canais lês e a que horas é '
        + 'saber a tua rotina; não fica em claro numa pasta.'));
      s2.append(elemento('p', 'nota',
        'Um canal conta-se como lido quando o abres. As tuas próprias mensagens nunca '
        + 'contam — a app não te avisa de que tu falaste.'));
      painel.append(s2);

      const s3 = seccao('O que não existe');
      const a = elemento('div', 'aviso');
      a.append(elemento('p', null,
        'Som de aviso, e avisos só quando alguém escreve o teu nome. O primeiro é trabalho '
        + 'a sério de mistura com o áudio da chamada, que já usa o dispositivo; o segundo '
        + 'precisa de decidir o que é «o teu nome» numa app onde as pessoas se chamam o que '
        + 'quiserem e a identidade é uma chave.'));
      a.append(elemento('p', null,
        'Avisos com a app fechada. Não há servidor a receber por ti: se o Bruma não está a '
        + 'correr, a mensagem espera na máquina de quem a escreveu.'));
      s3.append(a);
      painel.append(s3);
    },
  },

  cobrancas: {
    nome: 'Cobranças',
    grupo: 'Definições do utilizador',
    ico: '<rect x="2.4" y="4" width="11.2" height="8" rx="1.6"/><path d="M2.4 6.8h11.2"/>',
    vazia: true,
    desenha: painel => {
      painel.append(elemento('h2', null, 'Cobranças'));
      const d = elemento('div', 'def__sec');
      d.append(elemento('div', 'members__label', 'Não existe, e não vai existir'));
      const a = elemento('div', 'aviso');
      a.append(elemento('b', null, 'Não há nada a pagar.'));
      a.append(document.createTextNode(
        ' Isto não é uma funcionalidade em falta — é o desenho. Não há servidor para '
        + 'sustentar, não há armazenamento a alugar, e não há nada para vender: o Bruma '
        + 'corre entre os teus computadores e mais nada. Esta secção está aqui só para não '
        + 'ficares à procura dela.'));
      d.append(a);
      painel.append(d);
    },
  },

  voz: {
    nome: 'Voz e vídeo',
    grupo: 'Experiência',
    ico: '<rect x="6" y="2" width="4" height="7" rx="2"/><path d="M4 8a4 4 0 0 0 8 0M8 12v2"/>',
    desenha: painel => {
      painel.append(elemento('h2', null, 'Voz e vídeo'));

      const s1 = seccao('Microfone');

      // A ESCOLHA DE DISPOSITIVO, que não existia (#105). A secção «Câmara» aqui em baixo
      // era exemplar a admitir que não tinha escolha; esta calava a mesma limitação, e no
      // microfone ela é muito mais grave: uma câmara errada vê-se de imediato, um microfone
      // errado só se descobre quando alguém diz que não se ouve nada.
      const escolha = document.createElement('select');
      escolha.className = 'def__escolha';
      const porOmissao = document.createElement('option');
      porOmissao.value = '';
      porOmissao.textContent = 'O que o Windows tiver como predefinido';
      escolha.append(porOmissao);
      escolha.value = microfoneEscolhido();
      escolha.onchange = () => {
        localStorage.setItem(MICROFONE, escolha.value);
        // O resultado é sobre o dispositivo que foi testado. Deixá-lo no ecrã ao lado de
        // outro dispositivo é a mesma família de mentira que o `vozPartida` que não se
        // limpava: uma frase verdadeira sobre um mundo que já não é este.
        testeDoMicro = null;
        // Aplica-se JÁ, e não só na próxima chamada: um interruptor que só faz efeito
        // depois de sair e voltar a entrar é um interruptor que parece não funcionar.
        if (voz.canal) reabrirMicrofone('escolheste outro microfone', true);
        mostrarPainel('voz');
      };
      const linhaEscolha = elemento('div', 'def__linha');
      const rotulo = elemento('span');
      rotulo.append(elemento('b', null, 'Dispositivo'));
      rotulo.append(elemento('i', null,
        'A escolha fica guardada. Se o dispositivo desaparecer, volta-se ao predefinido e '
        + 'diz-se aqui.'));
      linhaEscolha.append(rotulo, escolha);
      s1.append(linhaEscolha);

      if (microfoneRecuado) {
        s1.append(elemento('div', 'def__valor',
          'O microfone que escolheste não está disponível agora — estás a usar o '
          + 'predefinido do Windows.'));
      }

      // A lista chega depois: `enumerateDevices` é assíncrono e o painel desenha-se já.
      // E os nomes só existem depois de o microfone ter sido autorizado uma vez — por isso
      // é que se diz isso em vez de mostrar uma lista de dispositivos sem nome.
      if (navigator.mediaDevices && navigator.mediaDevices.enumerateDevices) {
        navigator.mediaDevices.enumerateDevices().then(ds => {
          const micros = ds.filter(d => d.kind === 'audioinput' && d.deviceId !== 'default'
            && d.deviceId !== 'communications');
          for (const d of micros) {
            const o = document.createElement('option');
            o.value = d.deviceId;
            o.textContent = d.label || 'Microfone sem nome (autoriza-o uma vez para o veres)';
            escolha.append(o);
          }
          // Se o que estava guardado já não está na lista, a caixa não pode ficar a
          // mostrar o predefinido como se fosse a escolha — diz-se que ele sumiu.
          const guardado = microfoneEscolhido();
          if (guardado && !micros.some(d => d.deviceId === guardado)) {
            const o = document.createElement('option');
            o.value = guardado;
            o.textContent = 'O que escolheste (não está ligado agora)';
            escolha.append(o);
          }
          escolha.value = guardado;
        }).catch(() => {
          s1.append(elemento('div', 'def__valor',
            'Não consegui listar os microfones desta máquina.'));
        });
      }

      s1.append(interruptor(
        'Supressão de ruído',
        'Tira o ventilador e o teclado, e cancela o eco do que sai das tuas colunas para o '
        + 'teu microfone.',
        ruidoReal === null ? ruidoSuprimido : ruidoReal,
        // O valor vem da CAIXA, não de uma alternância cega: é o que o interruptor
        // acabou de mostrar à pessoa, e é a isso que ela está a responder.
        v => porRuido(v),
      ));
      // O QUE O DISPOSITIVO DIZ, e não o que se lhe pediu (#35, #191).
      if (ruidoReal !== null && ruidoReal !== ruidoSuprimido) {
        s1.append(elemento('div', 'def__valor',
          `Pediste ${ruidoSuprimido ? 'ligada' : 'desligada'} e o teu microfone está a `
          + 'fazer o contrário — ele não deixa mudar isto.'));
      } else if (voz.micro && ruidoReal === null) {
        s1.append(elemento('div', 'def__valor',
          'O teu microfone não diz se está mesmo a suprimir ruído, portanto não consigo '
          + 'confirmar que isto está a ser respeitado.'));
      }

      // O TESTE (#107).
      const bt = elemento('button', 'def__bt',
        testeACorrer ? 'A testar…' : 'Testar o microfone');
      bt.disabled = testeACorrer;
      bt.onclick = () => testarMicrofone();
      s1.append(bt);
      s1.append(elemento('p', 'nota',
        'Grava 3 segundos, passa-os pelo mesmo Opus que a chamada usa, e toca-os de volta — '
        + 'sem ninguém do outro lado. Prova três coisas de uma vez: o dispositivo capta, o '
        + 'codec desta máquina funciona, e a saída toca.'));
      if (testeDoMicro) {
        s1.append(elemento('div',
          testeDoMicro.estado === 'mau' ? 'aviso' : 'def__valor', testeDoMicro.texto));
      }
      painel.append(s1);

      const q = qualidadeDePartilha();
      const s2 = seccao('Partilha de ecrã');
      s2.append(interruptor(
        'Levar o som do sistema',
        'O que sai das tuas colunas segue com a imagem — e a tua própria chamada fica de '
        + 'fora, para ninguém se ouvir a si próprio.',
        q.som,
        v => guardarQualidade({ som: v }),
      ));
      s2.append(elemento('div', 'def__valor',
        `qualidade actual: ${q.altura === 0 ? 'nativa' : q.altura + 'p'} · ${q.fps} ips`
        + `${q.debito ? ' · ' + rotuloDe(OPCOES_DEBITO, q.debito) : ' · débito automático'}`));
      s2.append(elemento('p', 'nota',
        'A resolução e o ritmo escolhem-se na engrenagem, no momento de partilhar — porque '
        + 'a escolha depende do que vais mostrar. E são um TETO: nunca aumentam uma fonte '
        + 'mais pequena do que isso.'));
      painel.append(s2);

      const s3 = seccao('Câmara');
      s3.append(elemento('p', 'nota',
        'Liga-se e desliga-se na barra da chamada. Não há aqui escolha de dispositivo: usa '
        + 'a câmara que o Windows tiver como predefinida.'));
      painel.append(s3);
    },
  },

  aparencia: {
    nome: 'Aparência',
    grupo: 'Experiência',
    ico: '<circle cx="8" cy="8" r="5.6"/><path d="M8 2.4v11.2"/>',
    vazia: true,
    desenha: painel => {
      painel.append(elemento('h2', null, 'Aparência'));
      painel.append(aindaNaoHa('Há um tema só, e é escuro.',
        'A paleta é um azul frio dessaturado — névoa — escolhida de propósito para não ser '
        + 'o roxo do Discord. Um tema claro daria trabalho a fazer bem e ainda não foi '
        + 'feito; um mal feito seria pior do que não haver.'));
    },
  },

  acessibilidade: {
    nome: 'Acessibilidade',
    grupo: 'Experiência',
    ico: '<circle cx="8" cy="3.6" r="1.6"/><path d="M3 6.4h10M8 6.4v4M8 10.4 5.6 14M8 10.4 10.4 14"/>',
    desenha: painel => {
      painel.append(elemento('h2', null, 'Acessibilidade'));
      const s1 = seccao('Movimento');
      s1.append(interruptor(
        'Reduzir o movimento',
        'Pára a névoa do fundo e encurta as transições. A app já respeita a definição do '
        + 'Windows; isto força-a mesmo que o sistema não a peça.',
        localStorage.getItem(SEM_MOVIMENTO) === '1',
        v => { localStorage.setItem(SEM_MOVIMENTO, v ? '1' : '0'); aplicarMovimento(); },
      ));
      painel.append(s1);
    },
  },

  sistema: {
    nome: 'Sistema',
    grupo: 'Experiência',
    ico: '<rect x="2.4" y="3.2" width="11.2" height="7.6" rx="1.4"/><path d="M5.6 13.6h4.8"/>',
    desenha: async painel => {
      painel.append(elemento('h2', null, 'Sistema'));
      const sobre = await invoke('sobre_esta_instalacao').catch(() => null);
      const s1 = seccao('Esta instalação');
      if (sobre) {
        s1.append(elemento('div', 'def__valor', `Bruma ${sobre.versao}`));
        s1.append(elemento('p', 'nota',
          'Quando alguma coisa corre mal, o rasto fica aqui — e é a primeira coisa a olhar:'));
        s1.append(elemento('div', 'def__valor', sobre.registo));
        // O rasto do INSTALADOR pode estar noutra pasta (#178): a app escolhe onde regista
        // conforme onde vivem os dados, e o instalador escreve sempre ao lado do exe. Dizer
        // «olha para o registo» a apontar só para um deles escondia metade da história —
        // justamente a metade das actualizações.
        if (sobre.registo_do_instalador) {
          s1.append(elemento('p', 'nota', 'E o que o instalador fez a esta instalação:'));
          s1.append(elemento('div', 'def__valor', sobre.registo_do_instalador));
        }
      }
      const a1 = elemento('div', 'caixa__acoes');
      a1.style.justifyContent = 'flex-start';
      const proc = elemento('button', 'btn btn--primary', 'Procurar atualização');
      const nota = elemento('span', 'nota');
      proc.onclick = async () => {
        nota.textContent = 'a procurar…';
        nota.textContent = respostaDaProcura(await procurarAtualizacao());
      };
      const pasta = elemento('button', 'btn', 'Abrir a pasta');
      pasta.onclick = () => invoke('abrir_pasta_de_dados')
        .catch(e => { nota.textContent = `não consegui abrir: ${e}`; });
      a1.append(proc, pasta, nota);
      s1.append(a1);
      painel.append(s1);

      const s2 = seccao('Como se actualiza');
      s2.append(elemento('p', 'nota',
        'A app avisa quando há versão nova e nunca instala sem perguntar. A actualização é '
        + 'assinada, e a assinatura é verificada antes de se escrever seja o que for.'));
      painel.append(s2);
    },
  },

  idioma: {
    nome: 'Idioma e Horário',
    grupo: 'Experiência',
    ico: '<path d="M2.4 5.2h7M5.9 3.4v1.8M4.4 5.2c0 2.6 1.6 4.6 3.6 5.6M7.6 5.2c0 2-1.2 3.6-3 4.6"/><path d="M8.4 13.6 11 7.2l2.6 6.4M9.4 11.6h3.2"/>',
    vazia: true,
    desenha: painel => {
      painel.append(elemento('h2', null, 'Idioma e Horário'));
      painel.append(aindaNaoHa('A app só fala português.',
        'Não há tradução nem escolha de idioma. As horas seguem o relógio do Windows — '
        + 'fuso e formato, incluindo 12 ou 24 horas. '
        + 'Traduzir a app inteira é trabalho a sério, e não valia a pena antes de ela '
        + 'estar assente.'));
    },
  },

  sobreposicao: {
    nome: 'Sobreposição de jogo',
    grupo: 'Jogos e apps',
    ico: '<rect x="2" y="4.4" width="12" height="7.2" rx="2"/><path d="M5 8h2M6 7v2M10.2 8.4h.01M11.6 7h.01"/>',
    vazia: true,
    desenha: painel => {
      painel.append(elemento('h2', null, 'Sobreposição de jogo'));
      painel.append(aindaNaoHa('Não há sobreposição por cima do jogo.',
        'Desenhar por cima de um jogo em ecrã inteiro obriga a entrar no caminho gráfico '
        + 'dele, e é o tipo de coisa que faz jogos fechar e antivírus reclamar. Ainda não '
        + 'foi feito.'));
      const s = seccao('O que existe hoje');
      s.append(elemento('p', 'nota',
        'O Bruma reconhece o jogo que tens aberto e mostra-o no rodapé, para o partilhares '
        + 'num clique. Podes desligar isso em Dados e privacidade.'));
      painel.append(s);
    },
  },
};

const ORDEM = ['conta', 'dados', 'permissoes', 'notificacoes', 'cobrancas',
  'voz', 'aparencia', 'acessibilidade', 'sistema', 'idioma', 'sobreposicao'];

let painelActivo = 'conta';

/** As definições estão abertas E na secção da voz?
 *
 *  Havia três sítios a perguntar isto com `$('#defs').classList.contains('is-on')` — e essa
 *  classe **nunca é posta em lado nenhum**: o painel esconde-se com `hidden` (index.html) e
 *  o `abrirDefinicoes`/`fecharDefinicoes` só mexem nisso. A condição era permanentemente
 *  falsa, e o efeito era o pior possível: o `finally` do teste do microfone nunca
 *  redesenhava, portanto TODOS os caminhos de falha — sem `MediaStreamTrackProcessor`, sem
 *  microfone, microfone a entregar zeros — ficavam com «A gravar 3 segundos… fala agora.»
 *  para sempre e a razão nunca aparecia. Exactamente os casos por que o botão existe.
 *
 *  E confirma-se o painel: redesenhar o da voz quando a pessoa já navegou para outro
 *  arrancava-a de onde estava.
 */
function aVerAsDefinicoesDaVoz() {
  return !$('#defs').hidden && painelActivo === 'voz';
}

function desenharMenuDeDefinicoes(filtro = '') {
  const menu = $('#defs-menu');
  menu.textContent = '';
  const procura = filtro.trim().toLowerCase();
  let grupoActual = undefined;
  let algum = false;

  for (const chave of ORDEM) {
    const p = PAINEIS[chave];
    if (procura && !p.nome.toLowerCase().includes(procura)) continue;
    algum = true;
    // Os títulos de grupo só aparecem quando há alguma coisa por baixo deles — a filtrar,
    // um cabeçalho sozinho é ruído.
    if (!procura && p.grupo !== grupoActual) {
      grupoActual = p.grupo;
      if (p.grupo) menu.append(elemento('div', 'defs__grupo', p.grupo));
    }
    const b = elemento('button', 'defs__item' + (p.vazia ? ' is-vazia' : ''));
    b.innerHTML = `<svg viewBox="0 0 16 16" width="15" height="15">${p.ico}</svg>`;
    b.append(document.createTextNode(p.nome));
    b.classList.toggle('is-activa', chave === painelActivo);
    b.onclick = () => mostrarPainel(chave);
    menu.append(b);
  }
  if (!algum) menu.append(elemento('div', 'defs__nada', 'nada com esse nome'));
}

async function mostrarPainel(chave) {
  painelActivo = chave;
  const painel = $('#defs-painel');
  painel.textContent = '';
  await PAINEIS[chave].desenha(painel);
  painel.scrollTop = 0;
  $('#defs-conteudo').scrollTop = 0;   // secção nova começa no topo, e não a meio da anterior
  desenharMenuDeDefinicoes($('#defs-buscar').value);
}

async function abrirDefinicoes(qual) {
  const d = $('#defs');
  d.hidden = false;
  $('#defs-nome').textContent = vista.nome || '—';
  pintar($('#defs-avatar'), vista.eu || '');
  $('#defs-buscar').value = '';
  await mostrarPainel(qual || 'conta');
}

function fecharDefinicoes() { $('#defs').hidden = true; }

$('#defs-fechar').onclick = fecharDefinicoes;
$('#defs-editar').onclick = () => mostrarPainel('conta');
$('#defs-buscar').oninput = () => desenharMenuDeDefinicoes($('#defs-buscar').value);
document.addEventListener('keydown', ev => {
  if (ev.key === 'Escape' && !$('#defs').hidden) fecharDefinicoes();
});

/** O que dizer depois de procurar uma versão nova.
 *
 *  Está à parte por ser a única forma de provar, sem rede, que as três respostas são
 *  mesmo três -- e não duas com a falha disfarçada de boa notícia. */
function respostaDaProcura(r) {
  if (r === 'ha') return '';                       // a faixa já apareceu e diz tudo
  if (r === 'nao') return 'já estás na versão mais recente';
  return 'não consegui verificar — sem rede, ou o servidor está em baixo';
}

/** A secção das 24 palavras. Vive na Conta, que é onde alguém a iria procurar. */
function seccaoDasPalavras() {
  const d = seccao('As tuas 24 palavras');
  d.append(elemento('p', 'nota',
    'São a tua chave escrita de outra maneira. Com elas recuperas a identidade noutra '
    + 'máquina, ou nesta depois de formatar.'));
  const av = elemento('div', 'aviso');
  av.append(elemento('b', null, 'Quem tiver estas palavras é tu.'));
  av.append(document.createTextNode(
    ' Lê o teu histórico, entra nas tuas salas e fala em teu nome. Escreve-as num papel e '
    + 'guarda-o onde guardarias uma chave de casa.'));
  d.append(av);

  const caixa = elemento('div', 'palavras');
  caixa.hidden = true;
  d.append(caixa);
  const nota = elemento('span', 'nota');

  const acs = elemento('div', 'caixa__acoes');
  acs.style.justifyContent = 'flex-start';
  const ver = elemento('button', 'btn btn--primary', 'Mostrar as palavras');
  const copiar = elemento('button', 'btn', 'Copiar');
  copiar.hidden = true;
  ver.onclick = async () => {
    try {
      const texto = await invoke('palavras_da_identidade');
      caixa.textContent = '';
      texto.split(/\s+/).forEach((palavra, n) => {
        const linha = elemento('span');
        linha.append(elemento('i', null, String(n + 1).padStart(2, ' ')));
        linha.append(document.createTextNode(palavra));
        caixa.append(linha);
      });
      caixa.hidden = false;
      ver.hidden = true;
      copiar.hidden = false;
      nota.textContent = '';
    } catch (e) { nota.textContent = `não consegui: ${e}`; }
  };
  copiar.onclick = () => {
    const texto = [...caixa.children].map(l => l.textContent.replace(/^\s*\d+\s*/, '')).join(' ');
    navigator.clipboard.writeText(texto)
      .then(() => { nota.textContent = 'copiadas — cola-as num sítio só teu, e apaga a seguir'; })
      .catch(() => { nota.textContent = 'não consegui copiar; escreve-as à mão'; });
  };
  const restaurar = elemento('button', 'btn', 'Restaurar de outras palavras');
  acs.append(ver, copiar, restaurar, nota);
  d.append(acs);

  const cx = elemento('div');
  cx.hidden = true;
  const ta = document.createElement('textarea');
  ta.id = 'palavras-entrada';
  ta.rows = 3;
  ta.placeholder = 'palavra1 palavra2 palavra3 …';
  cx.append(ta);
  const perigo = elemento('div', 'aviso aviso--perigo');
  perigo.innerHTML = 'Isto <b>troca</b> a identidade desta máquina. Os servidores que tens '
    + 'aqui deixam de abrir — as chaves deles pertencem à identidade antiga. <b>Nada é '
    + 'apagado:</b> a identidade antiga e o índice antigo ficam guardados ao lado, com a data, '
    + 'e voltar a essas palavras traz tudo de volta. A app reinicia sozinha ao restaurar.';
  cx.append(perigo);
  const a2 = elemento('div', 'caixa__acoes');
  a2.style.justifyContent = 'flex-start';
  const fazer = elemento('button', 'btn btn--perigo', 'Restaurar');
  const nota2 = elemento('span', 'nota');
  nota2.id = 'restaurar-nota';
  fazer.onclick = async () => {
    const palavras = ta.value.trim();
    if (!palavras) { nota2.textContent = 'escreve as 24 palavras primeiro'; return; }
    nota2.textContent = 'a verificar…';
    fazer.disabled = true;
    ta.disabled = true;
    try {
      // O comando reinicia o processo e NÃO regressa (a semente em memória é a antiga, e
      // deixá-la a gravar corromperia o índice novo). Se chegar a devolver, é porque falhou
      // ANTES do reinício — aí mostra-se o erro e volta-se a permitir tentar.
      nota2.textContent = 'a restaurar e a reiniciar…';
      await invoke('restaurar_identidade', { palavras });
    } catch (e) {
      nota2.textContent = String(e);
      fazer.disabled = false;
      ta.disabled = false;
    }
  };
  a2.append(fazer, nota2);
  cx.append(a2);
  d.append(cx);
  restaurar.onclick = () => { cx.hidden = !cx.hidden; if (!cx.hidden) ta.focus(); };
  return d;
}

$('#ok-nome').onclick = async () => {
  const nome = $('#in-nome').value.trim();
  if (!nome) return erroEm('erro-nome', 'escreve um nome');
  try {
    await invoke('definir_nome', { nome });
    fechar('veu-bemvindo');
    await desenharTudo();
  } catch (e) { erroEm('erro-nome', String(e)); }
};

/** O tecto de uma mensagem, em caracteres.
 *
 *  O mesmo numero que o Rust impoe, e escrito aqui SO para o contador poder avisar antes de
 *  se carregar em Enter. Quem manda e o Rust: uma verificacao que so existe na interface e
 *  uma sugestao, porque o comando pode ser chamado de outro sitio.
 *
 *  Existe porque uma mensagem entra no log de toda a gente e nao se apaga. Sem tecto, uma
 *  colagem distraida de cinco megabytes fica no disco dos dois para sempre, a ser
 *  sincronizada em cada ligacao.
 */
const MAX_TEXTO = 4000;

/** Faz o campo crescer com o texto, ate ao tecto do CSS. */
function ajustarEntrada(el) {
  el.style.height = 'auto';
  el.style.height = `${el.scrollHeight}px`;
}

function atualizarConta(el) {
  const conta = $('#conta-texto');
  if (!conta) return;
  const falta = MAX_TEXTO - el.value.length;
  // So aparece perto do fim. Um contador sempre visivel num campo de conversa e ruido.
  if (falta > 300) { conta.textContent = ''; conta.className = 'composer__conta'; return; }
  conta.textContent = String(falta);
  conta.className = 'composer__conta ' + (falta < 0 ? 'passou' : 'perto');
}

$('#entrada').addEventListener('input', ev => {
  ajustarEntrada(ev.target);
  atualizarConta(ev.target);
});

$('#entrada').addEventListener('keydown', async ev => {
  // SHIFT+ENTER faz uma linha nova; ENTER envia.
  //
  // E nao o contrario, apesar de o campo ser agora multi-linha: numa conversa escreve-se
  // sobretudo uma linha de cada vez, e obrigar a um atalho para o caso comum e trocar o
  // frequente pelo raro.
  if (ev.key !== 'Enter' || ev.shiftKey) return;
  ev.preventDefault();
  const texto = ev.target.value;
  if (!texto.trim()) return;
  if (texto.length > MAX_TEXTO) {
    // Nao se corta o texto de ninguem em silencio: fica no campo, e o contador diz porque.
    atualizarConta(ev.target);
    return;
  }
  ev.target.value = '';
  ajustarEntrada(ev.target);
  atualizarConta(ev.target);
  const destino = destinoDeEscrita();
  if (!destino) return;
  try {
    await invoke('enviar', { ...destino, texto });
    await desenharMensagens();
  } catch (e) {
    // O texto volta para o campo: perder o que se escreveu por causa de um erro de rede e
    // pior do que o erro.
    ev.target.value = texto;
    ajustarEntrada(ev.target);
    console.error(e);
  }
});

/* ==========================================================================
   A voz, pelo mesmo caminho do ecrã.

   Isto ia por WebRTC, e o WebRTC precisa que alguém lhe diga por onde furar o router —
   um servidor STUN ou TURN, configurado à mão nas duas máquinas. Sem isso ele só encontra
   caminhos dentro da rede local, e entre duas casas não há nenhum. Era a última coisa no
   Bruma que exigia configuração para funcionar de todo.

   Agora o som é codificado aqui em Opus, entregue ao Rust, e vai pelo iroh — que já trata
   do NAT sozinho e já é o caminho das mensagens e do ecrã. Não há nada para configurar, e
   a voz deixa também de expor o endereço de quem fala.
   ========================================================================== */

const VOZ_HZ = 48000;
const VOZ_BITRATE = 24000;      // Opus a 24 kbps é transparente para fala
const VOZ_QUADRO_US = 20000;    // 20 ms por pedaço

/** Quanto som se guarda antes de o tocar.
 *
 *  A rede não entrega os pedaços com o espaçamento com que eles saíram: uns chegam
 *  atrasados, outros aos pares. Tocar cada um assim que chega dá estalidos. Guardam-se 80
 *  ms de folga — o suficiente para absorver a irregularidade normal, pouco o bastante para
 *  não se notar na conversa.
 */
const VOZ_FOLGA = 0.08;

/* A FOLGA DEIXA DE SER UM NÚMERO ESCRITO À MÃO (#104, #117).
 *
 * Os 80 ms foram escolhidos para «a irregularidade normal». Entre os EUA e o Brasil, e
 * sobretudo quando a ligação cai para relay, a irregularidade normal é outra — e o sintoma
 * é a voz a picar. Sobe depressa (a cada rajada de cortes), desce devagar (só ao fim de um
 * minuto inteiro sem nenhum), e nunca passa do tecto: uma folga que cresce e não encolhe
 * transforma a conversa num walkie-talkie, que é o mesmo problema com outro nome.
 */
const VOZ_FOLGA_TECTO = 0.20;
const VOZ_FOLGA_PASSO = 0.02;
/** Quantos cortes numa janela de 10 s justificam subir a folga. */
const CORTES_PARA_SUBIR = 2;
const JANELA_DOS_CORTES = 10000;
/** Quanto tempo limpo é preciso para descer um degrau. */
const LIMPO_PARA_DESCER = 60000;

/** Os cortes na reprodução, por pessoa (#65).
 *
 *  # Porque é que isto existe
 *
 *  `if (v.proximo < agora + 0.01 || v.proximo > agora + 0.6) v.proximo = agora + folga;` —
 *  cada vez que esta linha corria, a reprodução tinha sido reposta, ou porque ficou para trás
 *  ou porque secou. Isso OUVE-SE: é um estalo, ou uma sílaba que desaparece. E não deixava
 *  rasto nenhum: nem contador, nem registo, nem uma linha no painel. É exactamente o género
 *  de avaria que se descreve como «a voz está estranha» sem nada que a corrobore.
 *
 *  Vive fora do `voz.audio` de propósito: o `calarPeer` apaga a entrada de áudio quando a
 *  pessoa sai da sala, e a contagem de uma chamada não devia desaparecer com ela.
 */
const cortesDaVoz = new Map();

/** Os cortes dos últimos `quanto` ms — e a lista fica aparada.
 *
 *  A filtragem vivia DENTRO do ramo do corte, o que a tornava uma função de haver cortes
 *  novos e não de haver tempo passado: com 14 cortes e a rede a melhorar, o painel escrevia
 *  «som recomeçado 14x no último minuto» dez minutos depois do último. Aqui, quem lê é quem
 *  apara, portanto a janela é mesmo uma janela.
 */
function cortesDesdeHa(c, quanto) {
  const ms = performance.now();
  c.quando = c.quando.filter(t => ms - t < 60000);
  return quanto >= 60000 ? c.quando : c.quando.filter(t => ms - t < quanto);
}
function contaDosCortes(chave) {
  let c = cortesDaVoz.get(chave);
  if (!c) {
    // `limpoDesde` e `null` -- e nao zero -- de proposito. Zero e um INSTANTE valido no
    // relogio do `performance.now()`, e usa-lo tambem para dizer «nunca» faz as duas coisas
    // deixarem de se distinguir: nos primeiros 60 s de vida da app, «nunca houve corte»
    // passava a ler-se como «esta limpo desde o instante zero», que e uma afirmacao sobre o
    // passado que ninguem mediu.
    // `subiuEm` é `null` pelo MESMO motivo do `limpoDesde` — e ficou a zero na primeira
    // escrita, que é o defeito que esta fase já corrigiu duas vezes. Com zero,
    // `ms - 0 >= 10000` é falso nos primeiros dez segundos de vida da app: quem abrisse o
    // Bruma e entrasse logo numa chamada tinha a adaptação DESLIGADA exactamente na janela
    // em que a ligação ainda está a assentar.
    c = { folga: VOZ_FOLGA, quando: [], total: 0, saltado: 0, subiuEm: null, limpoDesde: null };
    cortesDaVoz.set(chave, c);
  }
  return c;
}

let vozCtx = null;
function contextoDeAudio() {
  if (!vozCtx) vozCtx = new AudioContext({ sampleRate: VOZ_HZ });
  if (vozCtx.state === 'suspended') vozCtx.resume();
  return vozCtx;
}

/* O RELÓGIO DA SAÍDA PODE PARAR, E ISSO ENGOLE A CHAMADA (#38).
 *
 * O `contextoDeAudio` só chama `resume()` no instante em que é invocado — e ele é invocado
 * uma vez por pessoa, quando ela aparece. Se o contexto for suspenso DEPOIS disso (o
 * dispositivo de saída desaparece, o Windows suspende, a política de autoplay volta a
 * morder), o `currentTime` deixa de avançar e o `tocar` continua a agendar `start()` contra
 * um relógio parado: nada se ouve, nada falha, e nada é dito.
 *
 * O vigia olha para o único sinal que não mente — se o relógio ANDOU. Duas voltas paradas
 * com a chamada aberta é uma avaria; uma pode ser escalonamento.
 */
let ultimoRelogioDaVoz = -1;
let voltasParado = 0;

/** Porque é que não estás a OUVIR, se não estás.
 *
 *  Separada do `vozFalhou` de propósito. O `vozFalhou` documenta-se como «porque é que a
 *  TUA VOZ não está a sair» e pinta o botão do microfone: escrever nele uma avaria da SAÍDA
 *  acendia o microfone a vermelho por causa dos altifalantes, e mandava procurar no sítio
 *  errado. Esta vive no botão de silenciar tudo, que é o lado certo do problema.
 */
let saidaFalhou = null;

setInterval(() => {
  if (!voz.canal || !vozCtx) { ultimoRelogioDaVoz = -1; voltasParado = 0; return; }
  const agora = vozCtx.currentTime;
  if (ultimoRelogioDaVoz >= 0 && agora === ultimoRelogioDaVoz) {
    voltasParado += 1;
    if (vozCtx.state === 'suspended') vozCtx.resume().catch(() => {});
    if (voltasParado >= 2 && !saidaFalhou) {
      // E o conselho é verdade PORQUE o `sairDeVoz` passou a fechar e a esquecer o
      // contexto. Antes, «sai e volta a entrar» reencontrava o mesmo `AudioContext`
      // parado — o `vozCtx` era atribuído uma vez e nunca mais voltava a `null`, portanto
      // o conselho mandava fazer uma coisa que não mudava nada.
      saidaFalhou = 'O som de saída parou — o dispositivo de saída mudou? '
        + 'Sai da chamada e volta a entrar.';
      desenharRodape();
    }
  } else if (agora !== ultimoRelogioDaVoz) {
    // Voltou a andar: se a queixa era esta, tira-se. Uma queixa que fica depois de o
    // problema passar é a mesma família do `vozPartida` que nunca se limpava.
    if (voltasParado > 0 && saidaFalhou) {
      saidaFalhou = null;
      desenharRodape();
    }
    voltasParado = 0;
  }
  ultimoRelogioDaVoz = agora;
}, 2000);

/* --- enviar ---------------------------------------------------------------- */

let envio = null;

/** Porque é que a tua voz não está a sair, se não está.
 *
 *  A câmara já tinha isto (`camaraFalhou`); a voz não tinha, e é a mais grave das duas:
 *  continuas na sala, com o microfone aceso e a aparecer presente, e **ninguém te ouve**.
 *  Sem uma palavra em lado nenhum, o sintoma do outro lado é "ele hoje está calado".
 */
let vozFalhou = null;

/** De quem é que deixaste de ouvir, e porquê. Um descodificador que morre tira-te UMA
 *  pessoa da chamada — ela continua a aparecer, e a suspeita cai nela. */
const vozPartida = new Map();

/** Quando é que o codificador entregou o último pedaço (#36).
 *
 *  É o que separa «estou a falar» de «estou a ser ouvido»: o microfone pode ter energia e
 *  não sair nada da máquina, e era esse o caso em que o anel verde mentia.
 */
let ultimoPedacoSaiu = 0;
/** Quanto tempo sem um pedaço a sair basta para o anel do próprio deixar de acender. */
const SAIDA_RECENTE_MS = 300;

/** O RMS mais alto do meu microfone nos últimos 15 s, e o instante em que o vi (#106).
 *
 *  O analisador já media isto oito vezes por segundo e o único uso que se lhe dava era
 *  acender ou apagar um anel. O caso mais comum de todos numa chamada — o microfone está
 *  aberto, não está silenciado, e entrega zeros porque o Windows o silenciou, porque tem o
 *  botão físico desligado, ou porque é o dispositivo errado — não produzia mensagem nenhuma.
 */
/** O último instante em que o meu microfone passou do chão — ou `null` se ainda não medi.
 *
 *  # Porque é que isto substituiu um par de variáveis
 *
 *  Havia um `picoDoMicro` (o máximo dos últimos 15 s) e um `picoVistoEm` (quando a janela
 *  rodou), e o leitor perguntava `agora - picoVistoEm > JANELA_DO_PICO`. Só que o ESCRITOR,
 *  em `medirFala`, repunha `picoVistoEm = agora` com a MESMA condição — `t - picoVistoEm >
 *  JANELA_DO_PICO` — oito vezes por segundo. Com o microfone a entregar zeros, a diferença
 *  nunca passava de ~15,12 s, e só estava acima dos 15 000 ms durante os ≤120 ms entre a
 *  janela rodar e o tique seguinte. O `desenharRodape` amostra de 3 em 3 s: a marca acendia
 *  umas 4% das vezes, uma vez em cada poucos minutos, e apagava-se no redesenho a seguir.
 *  Um microfone permanentemente morto dava um aviso a PISCAR em vez de um aviso.
 *
 *  Um relógio, um significado: aqui só se escreve quando há mesmo som. Quem lê subtrai e
 *  ninguém lhe mexe por baixo.
 */
let acimaDoChaoEm = null;
/** Abaixo disto não é «alguém calado»: é um microfone que não capta. */
const CHAO_DO_MICRO = 0.002;
/** Quanto tempo abaixo do chão é preciso para se dizer que o microfone não capta nada. */
const JANELA_DO_PICO = 15000;

function comecarAEnviarVoz(microfone) {
  pararDeEnviarVoz();
  const faixa = microfone && microfone.getAudioTracks()[0];

  // OS DOIS RETURNS CALADOS (#163).
  //
  // Aqui o `getUserMedia` JÁ correu: a luz do microfone acende, a app aparece presente e não
  // silenciada, e nem um byte é codificado. O autoteste das capacidades imprime
  // «MediaStreamTrackProcessor=não existe» para a consola do Rust — que a pessoa que carrega
  // no botão do microfone nunca vê.
  //
  // E fecha-se a faixa no segundo caso: uma luz de microfone acesa é uma promessa, e não há
  // nada que a cumpra nesta máquina.
  if (!faixa) {
    vozFalhou = 'O dispositivo abriu mas não deu nenhuma faixa de som.';
    desenharRodape(); desenharVoz();
    return;
  }
  if (typeof MediaStreamTrackProcessor === 'undefined') {
    vozFalhou = 'Esta versão do WebView2 não sabe entregar som ao codificador — a tua voz '
      + 'não sai daqui. Actualizar o Edge WebView2 resolve.';
    try { faixa.stop(); } catch (e) { /* já */ }
    desenharRodape(); desenharVoz();
    return;
  }

  let carimbo = 0;
  const codificador = new AudioEncoder({
    output: pedaco => {
      // O QUE O CODIFICADOR ENTREGOU (#36). O anel verde do próprio vinha do analisador
      // ligado à faixa CRUA do microfone — antes do codificador. Com o codificador morto ou
      // sem `MediaStreamTrackProcessor`, eu falava, via o meu anel a acender, e concluía
      // que estava a ser ouvido.
      //
      // E o que este carimbo NÃO cobre, dito à frente: ele é escrito aqui, antes do
      // `invoke`, portanto prova que o pedaço foi CODIFICADO e não que saiu da máquina. Com
      // o `send_datagram` a recusar tudo, o anel continua a acender. Esse caso tem
      // instrumento próprio e está do outro lado — o `voz_falhados` (#34) e a perda que o
      // outro lado calcula (#124) —, e é lá que se lê, não aqui.
      ultimoPedacoSaiu = performance.now();
      // Só se envia a quem está mesmo na sala. Falar para uma lista vazia não custa nada
      // e não se manda nada para lado nenhum.
      const gente = [...voz.presentes.entries()]
        .filter(([, c]) => c === voz.canal).map(([p]) => p);
      if (!gente.length) return;
      const bytes = new Uint8Array(pedaco.byteLength);
      pedaco.copyTo(bytes);
      // O `servidor` vai agora junto: o Rust filtra a lista pelo `participa` da sala,
      // porque a verdade sobre quem me ouve não pode viver só aqui (#138).
      invoke('enviar_voz', { servidor: voz.servidor, para: gente, dados: [...bytes] })
        .catch(() => {});
    },
    error: e => {
      console.warn('o codificador de voz parou:', e);
      vozFalhou = 'O codificador de voz desistiu — ninguém te ouve. Sai e volta a entrar.';
      pararDeEnviarVoz();
      desenharRodape();
      desenharVoz();
    },
  });
  codificador.configure({
    codec: 'opus',
    sampleRate: VOZ_HZ,
    numberOfChannels: 1,
    bitrate: VOZ_BITRATE,
    opus: { frameDuration: VOZ_QUADRO_US },
  });

  const leitor = new MediaStreamTrackProcessor({ track: faixa }).readable.getReader();
  // O LAÇO OLHA PARA O SEU PRÓPRIO ENVIO, e não para a variável de módulo.
  //
  // Isto foi um defeito a sério, e vale a pena dizer qual. O `while (envio && envio.vivo)`
  // e o `if (envio && envio.vivo)` liam a GLOBAL. Ao trocar de microfone, o
  // `comecarAEnviarVoz` novo corre `pararDeEnviarVoz()` — que põe `E1.vivo = false` e
  // `envio = null` — e chega a `envio = E2` **sem um único `await` pelo meio**. Só depois
  // é que a continuação do laço antigo acordava; e nessa altura `envio` já era o E2, com
  // `vivo === true`. Ou seja: cada troca de microfone bem sucedida escrevia «o teu
  // microfone deixou de entregar som» por cima do `vozFalhou = null` que a reabertura
  // acabara de limpar, e deixava o botão vermelho até se sair da chamada.
  const meu = { codificador, leitor, vivo: true };
  envio = meu;

  (async () => {
    while (meu.vivo) {
      const { value, done } = await leitor.read().catch(() => ({ done: true }));
      if (done) {
        // OS DOIS FINS DESTE LAÇO NÃO SÃO A MESMA COISA (#101).
        //
        // Se o `vivo` ainda for verdadeiro, ninguém pediu para parar: a faixa acabou
        // sozinha. É o que acontece quando os auscultadores saem, o dispositivo desaparece,
        // ou o Windows muda o predefinido. O laço saía calado, e a app continuava a dizer
        // que estavas a falar — com o anel a acender, porque ele media o microfone e não o
        // que sai da máquina.
        //
        // `envio === meu` é a segunda metade: só quem AINDA é o envio em uso pode dizer
        // que o microfone morreu. Um laço já substituído não fala por quem o substituiu.
        if (meu.vivo && envio === meu) {
          vozFalhou = 'O teu microfone deixou de entregar som (o dispositivo mudou ou '
            + 'desapareceu). A tentar reabrir…';
          desenharRodape(); desenharVoz();
          reabrirMicrofone('a faixa acabou sozinha');
        }
        break;
      }
      // O microfone silenciado não envia nada. Não basta baixar o volume: o que não sai
      // desta máquina é o que ninguém pode ouvir.
      const calado = !faixa.enabled;
      if (!calado && codificador.state === 'configured') {
        try { codificador.encode(value); } catch (e) { /* o próximo vai */ }
      }
      carimbo = value.timestamp;
      value.close();
    }
  })();
  void carimbo;
}

/** O que se pede ao microfone. Num sítio só, porque é pedido em três. */
/** Onde vive o microfone escolhido (#105). */
const MICROFONE = 'bruma.microfone';
/** Qual foi o último `deviceId` que se pediu e falhou, para não se insistir. */
let microfoneRecuado = null;

function microfoneEscolhido() {
  return localStorage.getItem(MICROFONE) || '';
}

/** Abre o microfone, e RECUA para o predefinido se o escolhido já não existir (#105).
 *
 *  # Porque é que isto não é só um `getUserMedia`
 *
 *  Com `deviceId: { exact: … }`, um dispositivo que desapareceu — os auscultadores que
 *  ficaram noutra casa, o dock que não está ligado — não faz o pedido recuar: faz o pedido
 *  **falhar** com `OverconstrainedError`. Sem apanhar isso, escolher um microfone uma vez
 *  trocava «o microfone errado» por «microfone nenhum», que é estritamente pior.
 *
 *  E o recuo diz-se em voz alta. Ficar calado a usar outro dispositivo é a mesma família de
 *  mentira que o #102: continua a captar, e a captar o sítio errado.
 */
async function abrirMicrofone() {
  const escolhido = microfoneEscolhido();
  if (escolhido) {
    try {
      const m = await navigator.mediaDevices.getUserMedia(pedidoDeMicrofone(escolhido));
      microfoneRecuado = null;
      return m;
    } catch (e) {
      const nome = e && e.name;
      if (nome !== 'OverconstrainedError' && nome !== 'NotFoundError') throw e;
      microfoneRecuado = escolhido;
      console.warn('o microfone escolhido não existe; a recuar para o predefinido', e);
    }
  } else {
    // NÃO HÁ ESCOLHA, LOGO NÃO HÁ RECUO. O `microfoneRecuado = null` vivia só dentro do
    // ramo de cima: quem escolhia um dispositivo desligado e depois voltava ao predefinido
    // continuava a ver «o microfone que escolheste não está disponível agora» — uma frase
    // sobre uma escolha que já não existe.
    microfoneRecuado = null;
  }
  return navigator.mediaDevices.getUserMedia(pedidoDeMicrofone(''));
}

function pedidoDeMicrofone(qual) {
  // O QUE FICOU GUARDADO É O QUE SE PEDE (#35). Antes pedia-se sempre `true`, seja qual
  // fosse o valor da variável — o interruptor mexia numa coisa e o microfone abria noutra.
  // E o `autoGainControl` entra aqui também: até agora só existia no `applyConstraints`,
  // portanto o primeiro microfone da sessão abria sempre com ele ligado.
  const audio = {
    echoCancellation: ruidoSuprimido,
    noiseSuppression: ruidoSuprimido,
    autoGainControl: ruidoSuprimido,
  };
  // `exact` e não uma preferência: uma preferência que o motor ignora dá exactamente o
  // sintoma que o #105 descreve — a app diz que está a usar um dispositivo e usa outro.
  // Melhor falhar e recuar com uma frase do que acertar por acaso.
  const qualUsar = qual === undefined ? microfoneEscolhido() : qual;
  if (qualUsar) audio.deviceId = { exact: qualUsar };
  return { audio };
}

/** Quando foi a última reabertura, para não entrar em ciclo. */
let ultimaReabertura = 0;
let reabrindo = false;

/** Reabre o microfone quando ele muda ou morre (#101, #102).
 *
 *  # As duas avarias que isto cobre
 *
 *  **A faixa acaba sozinha** (#101): os auscultadores saem, o dispositivo desaparece, o
 *  Windows muda o predefinido. O laço de envio saía calado e a app continuava a dizer que
 *  estavas a falar.
 *
 *  **A faixa continua viva no dispositivo ERRADO** (#102): não havia um único
 *  `devicechange` em toda a app. O `getUserMedia` era pedido uma vez, sem `deviceId`, e
 *  ficava agarrado ao que era predefinido nesse instante. Ligar os auscultadores a meio da
 *  chamada muda o predefinido do Windows — e a faixa aberta continua no antigo.
 *
 *  # Os dois cuidados
 *
 *  Um intervalo mínimo de 3 s: os docks USB fazem o dispositivo aparecer e desaparecer em
 *  rajada, e sem isto reabria-se três vezes e ouviam-se três cortes. E uma bandeira de
 *  reentrância, porque o `getUserMedia` é assíncrono e o `devicechange` chega várias vezes
 *  seguidas por uma única ligação física.
 */
async function reabrirMicrofone(porque, deliberado) {
  if (!voz.canal || reabrindo) return;
  const agora = performance.now();
  // O intervalo mínimo existe para as RAJADAS dos docks USB. Uma escolha feita à mão no
  // painel não é uma rajada — e engoli-la em silêncio fazia o selector parecer partido.
  if (!deliberado && agora - ultimaReabertura < 3000) return;
  ultimaReabertura = agora;
  reabrindo = true;
  const canalNaAltura = voz.canal;
  // ESTAVAS SILENCIADO ANTES? CONTINUAS SILENCIADO DEPOIS.
  //
  // Isto era um defeito de confiança, e dos maus. O «silenciado» não vive em variável
  // nenhuma — vive só no `faixa.enabled` da faixa aberta (`$('#btn-mic').onclick` faz
  // `t.enabled = !t.enabled`), e o «surdo» impõe `t.enabled = false`. As faixas de um
  // `getUserMedia` novo nascem SEMPRE com `enabled === true`, e não havia uma linha que o
  // repusesse. Silenciava-me para tossir, ligava os auscultadores, e voltava a transmitir
  // sem o saber — com o botão ainda riscado durante até três segundos, porque o
  // `is-cortado` só se recalcula no `desenharRodape` seguinte.
  //
  // Lê-se ANTES do `await`: a faixa antiga pode já ter sido parada quando lá chegarmos.
  const faixaAntiga = voz.micro ? voz.micro.getAudioTracks()[0] : null;
  const estavaCalado = !!faixaAntiga && !faixaAntiga.enabled;
  try {
    const antigo = voz.micro;
    const novo = await abrirMicrofone();
    // Saiu da sala enquanto se esperava: não se fica com um microfone aberto por engano.
    if (voz.canal !== canalNaAltura) {
      novo.getTracks().forEach(t => t.stop());
      return;
    }
    if (antigo) antigo.getTracks().forEach(t => t.stop());
    // E o surdo conta tanto como o silenciado: ficar a falar para uma chamada que não se
    // está a ouvir é exactamente o que o botão de silenciar tudo existe para impedir.
    const faixaNova = novo.getAudioTracks()[0];
    if (faixaNova) faixaNova.enabled = !estavaCalado && !surdo;
    voz.micro = novo;
    comecarAEnviarVoz(novo);
    vigiarAudio(voz.eu, novo);
    lerRuidoReal();
    vozFalhou = null;
    console.info('microfone reaberto:', porque);
  } catch (e) {
    vozFalhou = `Não consegui reabrir o microfone (${porque}): ${e && e.message ? e.message : e}`;
  } finally {
    reabrindo = false;
    desenharRodape();
    desenharVoz();
  }
}

/* O dispositivo mudou por baixo de nós (#102).
   Comparar o `deviceId` em uso com o predefinido de agora: se mudou, a faixa aberta está no
   dispositivo errado — continua a captar, e a captar o sítio errado, que é a forma de isto
   passar despercebido. */
if (navigator.mediaDevices && navigator.mediaDevices.addEventListener) {
  navigator.mediaDevices.addEventListener('devicechange', async () => {
    if (!voz.canal || !voz.micro) return;
    const faixa = voz.micro.getAudioTracks()[0];
    if (!faixa) return reabrirMicrofone('a faixa desapareceu');
    if (faixa.readyState === 'ended') return reabrirMicrofone('a faixa terminou');
    try {
      const emUso = faixa.getSettings().deviceId;
      const todos = await navigator.mediaDevices.enumerateDevices();
      // COM UM DISPOSITIVO FIXADO, O PREDEFINIDO NÃO INTERESSA (#105).
      //
      // A comparação abaixo é contra a entrada `default`. Se a pessoa escolheu o
      // dispositivo X no painel — que é o caso normal de quem usa o selector —, o `groupId`
      // do predefinido é PERMANENTEMENTE diferente do de X, e a condição passava a ser
      // sempre verdadeira: cada `devicechange` reabria o microfone e dava um corte audível,
      // por causa de uma diferença que o `exact: X` garante que é para existir.
      //
      // Com um fixado, o que importa é se ELE ainda lá está.
      const fixado = microfoneEscolhido();
      if (fixado) {
        const aindaLa = todos.some(d => d.kind === 'audioinput' && d.deviceId === fixado);
        if (!aindaLa) reabrirMicrofone('o microfone que escolheste desapareceu');
        return;
      }
      const agora = todos.find(d => d.kind === 'audioinput' && d.deviceId === 'default');
      // Só se compara quando há os dois lados. Sem `deviceId` no `getSettings` — e há
      // motores que não o dão — não se conclui nada, que é melhor do que reabrir à toa.
      if (emUso && agora && agora.groupId && faixa.getSettings().groupId
          && agora.groupId !== faixa.getSettings().groupId) {
        reabrirMicrofone('o dispositivo predefinido mudou');
      }
    } catch (e) { /* enumerateDevices pode falhar sem permissões; não se conclui nada */ }
  });
}

function pararDeEnviarVoz() {
  if (!envio) return;
  envio.vivo = false;
  try { envio.leitor.cancel(); } catch (e) { /* já fechado */ }
  try { if (envio.codificador.state !== 'closed') envio.codificador.close(); } catch (e) { /* idem */ }
  envio = null;
}

/* ==========================================================================
   A câmara.

   Vai pelo caminho da voz — codificada aqui, com WebCodecs — e não pelo do ecrã, que é
   captado em Rust. A razão é a barra: o `getDisplayMedia` faz o WebView2 desenhar "está a
   partilhar", e foi por isso que o ecrã teve de sair do navegador. O `getUserMedia` de uma
   CÂMARA não faz isso — é só para captura de ecrã. Portanto a câmara pode ficar aqui, onde
   já existe tudo o que ela precisa: abrir dispositivos, codificar e desenhar.

   E há uma segunda razão, mais forte: o ecrã é UM de cada vez e enche o painel; as câmaras
   são VÁRIAS ao mesmo tempo. Descodificar N fluxos em paralelo é coisa que o navegador faz
   sozinho e que teria de ser reescrita à mão do outro lado.
   ========================================================================== */

const CAM_LARGURA = 640;
const CAM_ALTURA = 360;
const CAM_IPS = 24;
const CAM_DEBITO = 400_000;      // 400 kbps chega para uma cara a 360p
/** De quanto em quanto tempo se manda um frame COMPLETO.
 *
 *  Os outros frames só descrevem o que mudou desde o anterior, portanto quem chega a meio
 *  não consegue descodificar nada até vir um completo. Dois segundos é o compromisso: mais
 *  curto gasta upload à toa, mais longo deixa quem entra a olhar para um quadrado preto.
 */
const CAM_CHAVE_MS = 2000;

let camaraEnvio = null;
/** Conta quantas vezes se ligou ou desligou a câmara.
 *
 *  Serve para uma corrida real: entre carregar no botão e o `getUserMedia` responder passam
 *  centenas de milissegundos, e nesse intervalo a pessoa pode sair da chamada. Sem isto, o
 *  stream chegava depois e ficava ligado — com a luz da câmara acesa e ninguém em sala.
 *  Uma luz de câmara acesa sem chamada é a pior coisa que esta app podia fazer.
 */
let geracaoDaCamara = 0;

/** Uma câmara desenhada por nós, para o teste de par.
 *
 *  Existe porque a máquina onde isto se desenvolve não tem câmara nenhuma — só uma virtual
 *  do OBS, que não arranca sem o OBS aberto. Sem uma fonte destas, o caminho da câmara
 *  entre duas instâncias ficava por provar à espera de hardware, que é a pior razão para
 *  deixar código por verificar.
 *
 *  O quadrado ANDA de propósito: uma imagem parada comprime para quase nada e provaria
 *  pouco — o que interessa é que saiam bytes a sério e cheguem inteiros ao outro lado.
 */
function camaraDesenhada() {
  const tela = document.createElement('canvas');
  tela.width = CAM_LARGURA;
  tela.height = CAM_ALTURA;
  const pincel = tela.getContext('2d');
  let x = 0;
  const pintar = () => {
    pincel.fillStyle = '#101418';
    pincel.fillRect(0, 0, tela.width, tela.height);
    pincel.fillStyle = '#7fd4c1';
    pincel.fillRect(x % (tela.width - 60), 40 + (x % 200), 60, 60);
    x += 11;
  };
  const relogio = setInterval(pintar, 1000 / CAM_IPS);
  pintar();
  const stream = tela.captureStream(CAM_IPS);
  stream.__parar = () => clearInterval(relogio);
  return stream;
}

/** Quem está na sala E percebe o que vamos mandar.
 *
 *  Mandar a câmara a quem não a sabe distinguir do ecrã não é "melhor do que nada": é pior.
 *  Aquele lado meteria os bytes no descodificador errado e mostraria lixo, sem saber porquê.
 *  Vazia significa que nada sai da máquina — que é a resposta certa quando ninguém percebe.
 */
function gentePresente() {
  return [...voz.presentes.entries()]
    .filter(([p, c]) => c === voz.canal && voz.entendeCamara.has(p))
    .map(([p]) => p);
}

async function comecarAEnviarCamara(fonte = null) {
  pararDeEnviarCamara();
  if (typeof VideoEncoder === 'undefined' || typeof MediaStreamTrackProcessor === 'undefined') {
    throw new Error('esta versão do WebView2 não traz o codificador de vídeo');
  }
  const minhaVez = ++geracaoDaCamara;
  const stream = fonte || await navigator.mediaDevices.getUserMedia({
    video: { width: CAM_LARGURA, height: CAM_ALTURA, frameRate: CAM_IPS },
    audio: false,
  });
  // Chegou tarde: alguém desligou a câmara ou saiu da chamada enquanto isto esperava.
  // Apaga-se a luz e desiste-se em silêncio — não é um erro, é uma decisão que mudou.
  if (minhaVez !== geracaoDaCamara) {
    stream.getTracks().forEach(t => t.stop());
    if (stream.__parar) stream.__parar();
    return null;
  }
  const faixa = stream.getVideoTracks()[0];
  if (!faixa) { stream.getTracks().forEach(t => t.stop()); throw new Error('sem câmara'); }

  let ultimaChave = 0;
  const codificador = new VideoEncoder({
    output: pedaco => {
      const gente = gentePresente();
      if (!gente.length) return;
      const bytes = new Uint8Array(pedaco.byteLength);
      pedaco.copyTo(bytes);
      invoke('enviar_camara', {
        para: gente, servidor: voz.servidor, canal: voz.canal, dados: [...bytes],
      }).catch(() => {});
    },
    error: e => console.warn('o codificador da câmara parou:', e),
  });
  codificador.configure({
    codec: 'avc1.42001f',            // Baseline 3.1 — o que qualquer máquina descodifica
    width: CAM_LARGURA,
    height: CAM_ALTURA,
    framerate: CAM_IPS,
    bitrate: CAM_DEBITO,
    latencyMode: 'realtime',
    // `annexb` e não `avc`: em annexb cada pedaço traz os seus próprios parâmetros e
    // descodifica-se sozinho. Em `avc` os parâmetros vão UMA vez, fora da banda, e quem
    // entrasse a meio da chamada nunca mais os via — o mesmo problema que o `tfdt`
    // resolveu no ecrã, e a mesma lição: numa transmissão, cada pedaço tem de se bastar.
    avc: { format: 'annexb' },
  });

  const leitor = new MediaStreamTrackProcessor({ track: faixa }).readable.getReader();
  camaraEnvio = { codificador, leitor, faixa, stream, vivo: true };

  (async () => {
    while (camaraEnvio && camaraEnvio.vivo) {
      const { value, done } = await leitor.read().catch(() => ({ done: true }));
      if (done) break;
      // Um codificador que morreu não volta. Continuar a puxar frames seria gastar CPU e
      // manter a luz acesa para não enviar nada a ninguém — e em silêncio, que é o pior.
      if (codificador.state !== 'configured') {
        value.close();
        console.warn('o codificador da câmara morreu; a desligar');
        camaraFalhou = 'A câmara parou sozinha — o codificador de vídeo desistiu.';
        pararDeEnviarCamara();
        anunciarEstado();
        desenharVoz();
        desenharRodape();
        break;
      }
      {
        const agora = performance.now();
        const chave = agora - ultimaChave >= CAM_CHAVE_MS;
        if (chave) ultimaChave = agora;
        // Se a fila cresce, o codificador não está a acompanhar. Largar o frame é melhor
        // do que o acumular: numa chamada ao vivo o que interessa é o presente.
        if (codificador.encodeQueueSize < 3) {
          try { codificador.encode(value, { keyFrame: chave }); } catch (e) { /* o próximo vai */ }
        }
      }
      value.close();
    }
  })();

  voz.camara = stream;
  return stream;
}

function pararDeEnviarCamara() {
  // Sobe SEMPRE, mesmo sem nada a parar: é o que faz um `getUserMedia` ainda a decorrer
  // perceber que já não é preciso.
  geracaoDaCamara += 1;
  if (!camaraEnvio) return;
  camaraEnvio.vivo = false;
  try { camaraEnvio.leitor.cancel(); } catch (e) { /* já fechado */ }
  try {
    if (camaraEnvio.codificador.state !== 'closed') camaraEnvio.codificador.close();
  } catch (e) { /* idem */ }
  // A luz da câmara só se apaga quando a faixa PARA. Deixar o stream vivo com o
  // codificador fechado seria a pior das combinações: não sai nada e a luz fica acesa.
  try { camaraEnvio.stream.getTracks().forEach(t => t.stop()); } catch (e) { /* idem */ }
  try { if (camaraEnvio.stream.__parar) camaraEnvio.stream.__parar(); } catch (e) { /* idem */ }
  camaraEnvio = null;
  voz.camara = null;
}

/* --- receber a câmara dos outros ------------------------------------------- */

/** Um descodificador por pessoa, e um <video> que se reaproveita entre redesenhos. */
const camarasRecebidas = new Map();

function camaraDe(chave) {
  let c = camarasRecebidas.get(chave);
  if (c) return c;

  const tela = document.createElement('canvas');
  tela.className = 'tile__video';
  tela.width = CAM_LARGURA;
  tela.height = CAM_ALTURA;
  const pincel = tela.getContext('2d');

  c = { tela, pincel, descodificador: null, frames: 0, esperaChave: true };
  c.descodificador = new VideoDecoder({
    output: quadro => {
      c.frames += 1;
      if (tela.width !== quadro.displayWidth || tela.height !== quadro.displayHeight) {
        tela.width = quadro.displayWidth;
        tela.height = quadro.displayHeight;
      }
      try { pincel.drawImage(quadro, 0, 0, tela.width, tela.height); } catch (e) { /* segue */ }
      quadro.close();
    },
    error: e => {
      console.warn('descodificador da câmara de', chave, e);
      // Um erro deixa o descodificador inutilizável: exige-se um frame completo antes de
      // se lhe voltar a dar seja o que for, senão entra num ciclo de queixas.
      c.esperaChave = true;
    },
  });
  c.descodificador.configure({ codec: 'avc1.42001f', optimizeForLatency: true });
  camarasRecebidas.set(chave, c);
  return c;
}

(function ligarEntradaDeCamara() {
  if (!window.__TAURI__) return;
  const canal = new window.__TAURI__.core.Channel();
  canal.onmessage = pedaco => {
    const bytes = new Uint8Array(pedaco);
    if (!bytes.length) return;
    const n = bytes[0];
    if (bytes.length < 1 + n) return;
    const chave = new TextDecoder().decode(bytes.subarray(1, 1 + n));
    const corpo = bytes.subarray(1 + n);
    const c = camaraDe(chave);
    if (c.descodificador.state !== 'configured') return;

    // Em annexb, um frame completo traz o SPS (nal 7) à frente. Quem chega a meio tem de
    // esperar por um; dar-lhe um frame de diferenças é pedir imagem partida.
    const completo = temSPS(corpo);
    if (c.esperaChave) {
      if (!completo) return;
      c.esperaChave = false;
    }
    const primeiro = c.frames === 0;
    try {
      c.descodificador.decode(new EncodedVideoChunk({
        type: completo ? 'key' : 'delta',
        timestamp: performance.now() * 1000,
        data: corpo,
      }));
    } catch (e) {
      c.esperaChave = true;
      return;
    }
    if (primeiro) desenharVoz();   // o painel só sabe que há imagem depois do primeiro
  };
  invoke('receber_camara', { canal }).catch(() => {});
})();

/** Se este pedaço traz um SPS — ou seja, se é um frame que se descodifica sozinho.
 *
 *  Procura-se o código de início (00 00 01) e olha-se para os 5 bits baixos do byte
 *  seguinte: 7 é SPS. É mais barato e mais fiável do que confiar no que o codificador
 *  disse, porque o que chega ao outro lado são só bytes.
 */
function temSPS(bytes) {
  for (let i = 0; i + 3 < bytes.length; i++) {
    if (bytes[i] === 0 && bytes[i + 1] === 0 && bytes[i + 2] === 1) {
      const tipo = bytes[i + 3] & 0x1f;
      if (tipo === 7) return true;
      if (tipo === 1 || tipo === 5) return tipo === 5;
    }
  }
  return false;
}

/** Quem desligou a câmara deixa de ter descodificador. Não é só arrumação: um
 *  descodificador aberto continua a segurar memória de vídeo, e numa sala onde as pessoas
 *  vão ligando e desligando isso só cresce. */
/** A própria imagem, sem passar pela rede.
 *
 *  Espelhada, como em toda a gente: a pessoa está a ver-se a si, e ver-se ao contrário do
 *  espelho do quarto é desconcertante. Quem recebe vê a imagem na posição certa — a
 *  inversão é só local, no CSS.
 */
let espelho = null;
function meuEspelho() {
  if (!voz.camara) {
    if (espelho) { espelho.srcObject = null; espelho = null; }
    return null;
  }
  if (!espelho) {
    espelho = document.createElement('video');
    espelho.className = 'tile__video tile__video--espelho';
    espelho.autoplay = true;
    espelho.muted = true;      // é a nossa própria câmara; não tem som e não se ouve
    espelho.playsInline = true;
  }
  if (espelho.srcObject !== voz.camara) espelho.srcObject = voz.camara;
  return espelho;
}

function fecharCamaraRecebida(chave) {
  const c = camarasRecebidas.get(chave);
  if (!c) return;
  try {
    if (c.descodificador.state !== 'closed') c.descodificador.close();
  } catch (e) { /* já fechado */ }
  camarasRecebidas.delete(chave);
}

/* --- receber --------------------------------------------------------------- */

function vozDe(chave) {
  let v = voz.audio.get(chave);
  if (v) return v;

  const ctx = contextoDeAudio();
  const ganho = ctx.createGain();
  ganho.connect(ctx.destination);

  v = { ganho, comp: null, proximo: 0, descodificador: null, ctx, refeitos: [],
    corte: contaDosCortes(chave) };
  v.descodificador = novoDescodificador(chave, v);
  voz.audio.set(chave, v);
  ajustarVolume(chave);
  return v;
}

/** Quantas vezes se recria um descodificador partido antes de desistir, por minuto. */
const REFAZER_NO_MINUTO = 3;

/** Um descodificador de voz para uma pessoa — e o que fazer quando ele morre (#37).
 *
 *  # A avaria que isto corrige
 *
 *  O descodificador era criado UMA vez. No `error:` marcava-se `vozPartida` e mais nada: ele
 *  ficava em estado de erro, e a partir daí a chegada de voz dessa pessoa era toda deitada
 *  fora (`if (v.descodificador.state !== 'configured') return;`) — calada e permanentemente.
 *  Um erro transitório calava uma pessoa **até ao fim da chamada**, e a marca `vozPartida`
 *  também nunca era limpa por pessoa: só ao entrar noutra sala.
 *
 *  Recriar é seguro porque no Opus todos os pedaços se bastam a si próprios — não há estado
 *  entre eles que se perca. O que se perde é o pedaço que estava a ser descodificado quando
 *  falhou, e isso já estava perdido.
 *
 *  O tecto de três por minuto existe para a causa PERMANENTE — um formato que esta máquina
 *  não lê — não virar um ciclo a gastar CPU. Quando ele é atingido, a marca fica com a razão
 *  escrita, que é a informação que a pessoa precisa.
 */
function novoDescodificador(chave, v) {
  const d = new AudioDecoder({
    output: som => tocar(chave, som),
    error: e => {
      console.warn('descodificador de voz de', chave, e);
      const agora = performance.now();
      v.refeitos = (v.refeitos || []).filter(t => agora - t < 60000);
      if (v.refeitos.length >= REFAZER_NO_MINUTO) {
        vozPartida.set(chave, 'o áudio desta pessoa não descodifica nesta máquina — '
          + `desisti ao fim de ${REFAZER_NO_MINUTO} tentativas`);
        desenharVoz();
        return;
      }
      v.refeitos.push(agora);
      try { if (d.state !== 'closed') d.close(); } catch (err) { /* já */ }
      // Só se substitui se este AINDA for o descodificador em uso: um `calarPeer` a meio,
      // ou um segundo erro do mesmo, deixaria dois a escrever no mesmo sítio.
      if (voz.audio.get(chave) === v && v.descodificador === d) {
        v.descodificador = novoDescodificador(chave, v);
        vozPartida.set(chave, 'o áudio desta pessoa falhou e está a ser retomado');
        desenharVoz();
      }
    },
  });
  d.configure({ codec: 'opus', sampleRate: VOZ_HZ, numberOfChannels: 1 });
  return d;
}

function tocar(chave, som) {
  const v = voz.audio.get(chave);
  if (!v) { som.close(); return; }
  const ctx = v.ctx;

  const amostras = new Float32Array(som.numberOfFrames);
  try {
    som.copyTo(amostras, { planeIndex: 0, format: 'f32-planar' });
  } catch (e) {
    som.close();
    return;
  }
  som.close();

  // VOLTOU A SAIR SOM: a marca conta o PRESENTE e não o passado (#37).
  //
  // O `vozPartida` só era limpo ao entrar noutra sala. Depois de um erro transitório, a
  // etiqueta «sem áudio» ficava colada à pessoa para o resto da chamada, mesmo com a voz
  // dela a chegar outra vez — a dizer o contrário do que estava a acontecer.
  if (vozPartida.has(chave)) {
    vozPartida.delete(chave);
    desenharVoz();
  }

  // O anel verde de quem fala sai daqui: já se está a olhar para as amostras, não vale a
  // pena montar um analisador em paralelo só para as medir outra vez.
  medirNasAmostras(chave, amostras);

  const buffer = ctx.createBuffer(1, amostras.length, VOZ_HZ);
  buffer.copyToChannel(amostras, 0);
  const fonte = ctx.createBufferSource();
  fonte.buffer = buffer;
  fonte.connect(v.ganho);

  const agora = ctx.currentTime;
  const c = v.corte;
  const ms = performance.now();
  // Se ficámos para trás (a app esteve minimizada, a rede engasgou), não se tenta
  // recuperar o atraso a tocar tudo de enfiada: numa conversa ao vivo o que interessa é o
  // presente. Recomeça-se com a folga em uso.
  if (v.proximo < agora + 0.01 || v.proximo > agora + 0.6) {
    // O PRIMEIRO PEDAÇO NÃO É UM CORTE. Com `proximo` a zero esta condição é sempre
    // verdadeira, e contá-la daria um corte a toda a gente que entra numa sala — um
    // contador que acusa uma avaria em todas as chamadas é um contador que se aprende a
    // ignorar, e deixa de servir para a avaria a sério.
    if (v.proximo > 0) {
      c.total += 1;
      // Quanto som se saltou (ou se repetiu): a distância entre onde a reprodução ia e
      // onde ela recomeça. É a duração do buraco que se ouviu.
      //
      // COM UM TECTO POR CORTE, e a razão é honestidade e não defesa. Se a app esteve
      // minimizada dez minutos, esta distância dá 600 segundos — e o painel escreveria
      // «600 s saltados», que se lê como uma avaria de áudio catastrófica quando o que
      // houve foi ninguém estar a ouvir. Dois segundos é mais do que qualquer buraco que
      // uma conversa ao vivo possa ter e ainda ser uma conversa.
      c.saltado += Math.min(2, Math.abs(v.proximo - (agora + c.folga)));
      c.quando.push(ms);
      const em10s = cortesDesdeHa(c, JANELA_DOS_CORTES).length;
      // Sobe no máximo um degrau por janela: uma rajada de seis cortes seguidos é UMA
      // avaria, não seis, e subir seis degraus de uma vez atirava a folga para o tecto por
      // causa de um engasgo de um segundo.
      if (em10s >= CORTES_PARA_SUBIR && c.folga < VOZ_FOLGA_TECTO
          && (c.subiuEm === null || ms - c.subiuEm >= JANELA_DOS_CORTES)) {
        c.folga = Math.min(VOZ_FOLGA_TECTO, +(c.folga + VOZ_FOLGA_PASSO).toFixed(3));
        c.subiuEm = ms;
      }
      // O relógio do «limpo» recomeça a cada corte: é isso que faz um minuto sem cortes ser
      // mesmo um minuto sem cortes, e não um minuto desde o primeiro deles.
      c.limpoDesde = ms;
    }
    v.proximo = agora + c.folga;
  } else if (c.folga > VOZ_FOLGA && c.limpoDesde !== null
      && ms - c.limpoDesde >= LIMPO_PARA_DESCER) {
    // A DESCIDA TEM DE SER GARANTIDA, e por isso vive no caminho que corre 50 vezes por
    // segundo — não num temporizador que se pode nunca ter armado. Um minuto inteiro sem um
    // único corte devolve um degrau; o atraso que se acrescentou por causa de uma rede má
    // não fica lá para sempre depois de ela melhorar.
    c.folga = Math.max(VOZ_FOLGA, +(c.folga - VOZ_FOLGA_PASSO).toFixed(3));
    c.limpoDesde = ms;
    // E O ATRASO ENCOLHE MESMO. Sem esta linha, a folga descia no contador e não no
    // ouvido: o `v.proximo` só era reescrito no ramo do CORTE, portanto os 20 ms que a
    // adaptação acabou de devolver continuavam agendados até ao corte seguinte — que,
    // numa rede que melhorou, pode não voltar a acontecer. A conversa ficava com o atraso
    // de uma rede má e um painel a dizer que já não estava lá.
    //
    // Nunca abaixo de `agora + folga`: o que se recupera é a dianteira acumulada, não o
    // direito a agendar som para um instante que já passou.
    v.proximo = Math.max(agora + c.folga, v.proximo - VOZ_FOLGA_PASSO);
  }
  fonte.start(v.proximo);
  v.proximo += buffer.duration;
}

function calarPeer(chave) {
  const v = voz.audio.get(chave);
  if (!v) return;
  try { if (v.descodificador.state !== 'closed') v.descodificador.close(); } catch (e) { /* já */ }
  try { v.ganho.disconnect(); } catch (e) { /* já */ }
  // O limitador também: um nó que fica ligado ao destino depois de a pessoa sair é um nó
  // que ninguém volta a alcançar para desligar (#164).
  if (v.comp) { try { v.comp.disconnect(); } catch (e) { /* já */ } }
  voz.audio.delete(chave);
  voz.falando.delete(chave);
}

/** Onde vivem os volumes por pessoa (#164). */
const VOLUMES = 'bruma.volumes';

/** Os volumes, lidos UMA vez e guardados em memória.
 *
 *  O `oninput` do deslizador dispara a cada pixel de arrasto. Sem esta cache, cada um desses
 *  disparos fazia um `JSON.parse`, um `JSON.stringify` e um `setItem` — que é síncrono e vai
 *  ao disco — dezenas de vezes por segundo, a meio de uma chamada.
 */
let volumesEmMemoria = null;

function volumesGuardados() {
  if (volumesEmMemoria) return volumesEmMemoria;
  let lido = {};
  try { lido = JSON.parse(localStorage.getItem(VOLUMES) || '{}') || {}; }
  catch (e) { lido = {}; }
  // O que está no disco pode vir de uma mexida à mão, de uma versão futura, ou de um
  // ficheiro meio escrito. Filtra-se à entrada, e não a cada leitura: um valor de 900 num
  // `GainNode` não é um volume alto, é um estouro nas colunas de quem ouve.
  volumesEmMemoria = {};
  if (lido && typeof lido === 'object' && !Array.isArray(lido)) {
    for (const [k, v] of Object.entries(lido)) {
      if (typeof v === 'number' && Number.isFinite(v) && v >= 0 && v <= 2 && v !== 1) {
        volumesEmMemoria[k] = v;
      }
    }
  }
  return volumesEmMemoria;
}

/** O volume escolhido para uma pessoa, de 0 a 2. Um é o normal. */
function volumeDe(chave) {
  const v = volumesGuardados()[chave];
  return typeof v === 'number' ? v : 1;
}

/** Quando é que o disco fica em dia. O som muda JÁ; só a escrita é que espera. */
let gravarVolumes = null;

function guardarVolume(chave, valor) {
  const todos = volumesGuardados();
  // Um volume normal não se guarda: um ficheiro que enche com «1» por cada pessoa que
  // alguma vez esteve numa sala é ruído que nunca mais sai de lá.
  if (valor === 1) delete todos[chave]; else todos[chave] = valor;
  // O ganho aplica-se no mesmo instante — quem arrasta o deslizador quer ouvir o resultado
  // enquanto arrasta. O que se adia é só o `setItem`.
  ajustarVolume(chave);
  if (gravarVolumes) clearTimeout(gravarVolumes);
  gravarVolumes = setTimeout(() => {
    gravarVolumes = null;
    try { localStorage.setItem(VOLUMES, JSON.stringify(volumesEmMemoria || {})); }
    catch (e) { console.warn('não consegui guardar os volumes:', e); }
  }, 400);
}

/** O limitador que impede a amplificação de virar distorção (#164).
 *
 *  # Porque é que só se liga acima de 100%
 *
 *  Um ganho acima de 1 RECORTA: sem limitador, a «solução» soa pior do que o problema que
 *  veio resolver. Mas um `DynamicsCompressorNode` custa uns milissegundos de atraso — e numa
 *  chamada entre os EUA e o Brasil não se acrescenta atraso a toda a gente por causa de uma
 *  pessoa que subiu o volume de outra. Por isso o caminho normal fica exactamente como
 *  estava, e o nó só entra quando há mesmo o que limitar.
 */
function ligarLimitador(v, precisa) {
  if (precisa === !!v.comp) return;
  try { v.ganho.disconnect(); } catch (e) { /* já */ }
  if (precisa) {
    const c = v.ctx.createDynamicsCompressor();
    c.threshold.value = -6;
    c.knee.value = 12;
    c.ratio.value = 12;
    c.attack.value = 0.003;
    c.release.value = 0.25;
    v.ganho.connect(c);
    c.connect(v.ctx.destination);
    v.comp = c;
  } else {
    if (v.comp) { try { v.comp.disconnect(); } catch (e) { /* já */ } }
    v.comp = null;
    v.ganho.connect(v.ctx.destination);
  }
}

/** O volume de uma pessoa: zero se estivermos surdos ou se ela estiver silenciada.
 *
 *  Havia um `GainNode` por pessoa, montado e pronto, a ser usado como INTERRUPTOR: `? 0 : 1`
 *  (#164). Numa chamada de duas pessoas era precisamente o que faltava — se o amigo tem o
 *  ganho baixo do lado dele, a única resposta que a app dava era «silencia-o».
 */
function ajustarVolume(chave) {
  const v = voz.audio.get(chave);
  if (!v) return;
  const vol = (surdo || voz.silenciados.has(chave)) ? 0 : volumeDe(chave);
  v.ganho.gain.value = vol;
  ligarLimitador(v, vol > 1);
}

function ajustarTodosOsVolumes() {
  for (const chave of voz.audio.keys()) ajustarVolume(chave);
}

/* Os pedaços chegam do Rust com a chave de quem falou à frente. */
(function ligarEntradaDeVoz() {
  if (!window.__TAURI__) return;
  const canal = new window.__TAURI__.core.Channel();
  canal.onmessage = pedaco => {
    const bytes = pedaco instanceof ArrayBuffer ? new Uint8Array(pedaco) : new Uint8Array(pedaco);
    if (!bytes.length) return;
    const n = bytes[0];
    if (bytes.length < 1 + n) return;
    const chave = new TextDecoder().decode(bytes.subarray(1, 1 + n));
    if (!voz.canal) return;                 // não estamos numa sala: ignora-se
    // E tem de estar NESTA sala. O único filtro era «eu estou numa chamada» — não «quem
    // manda também está». Um datagrama de voz não leva sala nenhuma consigo, é só uma chave
    // e bytes, portanto quem passe o porteiro do Rust falava em qualquer conversa minha,
    // mesmo sem lá estar. A lista de presentes é o que distingue quem está de quem não está.
    if (voz.presentes.get(chave) !== voz.canal) return;
    const v = vozDe(chave);
    // O TECTO ERA «TRÊS E ACABOU PARA SEMPRE», e a constante chama-se `REFAZER_NO_MINUTO`.
    //
    // Ao atingir o tecto, o `error:` desiste e não recria — e a partir daí não há mais
    // erros, porque um descodificador morto não produz nenhum. Logo a lista `refeitos`
    // nunca mais era filtrada e o minuto nunca mais passava: a pessoa ficava calada até ao
    // fim da chamada, que é exactamente o defeito que o #37 veio corrigir.
    //
    // Aqui é o único sítio que continua a correr depois da desistência: cada pedaço que
    // chega passa por esta linha. Se o minuto passou, tenta-se outra vez.
    if (v.descodificador.state !== 'configured') {
      const agora = performance.now();
      const recentes = (v.refeitos || []).filter(t => agora - t < 60000);
      if (recentes.length >= REFAZER_NO_MINUTO) return;
      v.refeitos = recentes;
      try {
        if (v.descodificador.state !== 'closed') v.descodificador.close();
      } catch (e) { /* já */ }
      v.descodificador = novoDescodificador(chave, v);
      vozPartida.set(chave, 'o áudio desta pessoa falhou e está a ser retomado');
      desenharVoz();
      if (v.descodificador.state !== 'configured') return;
    }
    try {
      v.descodificador.decode(new EncodedAudioChunk({
        type: 'key',                        // no Opus todos os pedaços se bastam a si
        timestamp: performance.now() * 1000,
        data: bytes.subarray(1 + n),
      }));
    } catch (e) { /* um pedaço perdido não vale um erro */ }
  };
  invoke('receber_voz', { canal }).catch(() => {});
})();

/* ---------- eventos vindos do núcleo ---------- */

/* ---------- avisos do sistema -------------------------------------------------------
 *
 * O QUE VAI NO AVISO, E PORQUE E QUE O TEXTO NAO VAI POR OMISSAO.
 *
 * Um aviso do Windows nao e a app: ele aparece no ecra bloqueado, fica no historico de
 * notificacoes, e e lido por quem passar ao pe do computador. A app inteira existe para o
 * conteudo nao sair cifrado de ponta a ponta -- e depois copiava-o para uma superficie do
 * sistema operativo, onde nada disto vale.
 *
 * Por isso, por omissao, o aviso diz QUEM e onde, e nao O QUE. Quem quiser o texto liga-o
 * nas Definicoes, e la esta escrito o que isso custa.
 */
const AVISOS = 'bruma.avisos';         // '0' desliga
const AVISOS_TEXTO = 'bruma.avisos.texto';  // '1' mostra o texto da mensagem

function avisosLigados() { return localStorage.getItem(AVISOS) !== '0'; }
function avisosComTexto() { return localStorage.getItem(AVISOS_TEXTO) === '1'; }

let permissaoDeAviso = null;

async function avisar(titulo, corpo) {
  if (!avisosLigados()) return false;
  const api = window.__TAURI__ && window.__TAURI__.notification;
  if (!api) return false;
  try {
    if (permissaoDeAviso === null) {
      permissaoDeAviso = await api.isPermissionGranted();
      if (!permissaoDeAviso) {
        permissaoDeAviso = (await api.requestPermission()) === 'granted';
      }
    }
    if (!permissaoDeAviso) return false;
    await api.sendNotification({ title: titulo, body: corpo });
    return true;
  } catch (e) {
    // Um aviso que falha nao pode levar a app com ele: isto corre a cada mensagem.
    console.warn('aviso do sistema falhou', e);
    return false;
  }
}

/** O que ficou por ler, por sitio, para se saber o que e NOVO entre dois estados. */
function fotoDoPorLer(v) {
  const m = new Map();
  for (const s of (v && v.servidores) || []) {
    for (const [canal, n] of Object.entries(s.nao_lidos || {})) {
      const nome = (s.canais.find(c => c.id === canal) || {}).nome || canal;
      m.set(`s:${s.id}/${canal}`,
        { n, onde: `#${nome}`, quem: s.nome, servidor: s.id, canal });
    }
  }
  for (const c of (v && v.conversas) || []) {
    if (c.nao_lidos) {
      m.set(`c:${c.id}`,
        { n: c.nao_lidos, onde: 'mensagem privada', quem: c.nome, servidor: c.id, canal: c.canal });
    }
  }
  return m;
}

/** O que vai no corpo do aviso do sistema.
 *
 *  À parte, e não escrito no meio do `talvezAvisar`, porque é a DECISÃO em que assenta a
 *  promessa de privacidade — «por omissão, o texto não sai da app». Uma promessa dessas tem
 *  de ser mensurável sozinha, sem depender de o Windows mostrar seja o que for.
 *
 *  O `texto` já chega a `null` quando a opção está desligada; a verificação aqui é o cinto
 *  de segurança, para o dia em que alguém chamar isto de outro sítio.
 */
function corpoDoAviso(onde, texto) {
  if (avisosComTexto() && texto) return texto;
  return `Tens mensagens novas em ${umaLinha(onde, 60)}.`;
}

/** Uma linha só, e curta.
 *
 *  O CORPO do aviso tinha o cinto de segurança do `avisosComTexto`. O TÍTULO não tinha nada
 *  — e o título é o nome que a outra pessoa escolheu para si própria ou para a sala, texto
 *  livre que ela controla. Alguém que se chame a si próprio com três parágrafos põe três
 *  parágrafos no ecrã bloqueado de quem o tem na lista, com a opção «mostrar texto»
 *  desligada e a app a prometer que não mostra texto.
 *
 *  Uma linha e 40 caracteres fazem aquilo parecer o que é: um nome.
 */
function umaLinha(t, max) {
  // Sem literal de regex: qualquer coisa que passe por escapagem de camadas acaba com
  // uma nova linha REAL dentro da expressao, e a app fica sem carregar. Aqui olha-se
  // para os codigos, que nao tem escapes nenhuns.
  let limpo = '';
  let espaco = false;
  for (const c of String(t == null ? '' : t)) {
    const n = c.codePointAt(0);
    // Controlo (nova linha, tabulacao, e o resto do bloco C0/C1) vira um espaco so.
    if (n < 32 || (n >= 127 && n < 160)) { espaco = limpo.length > 0; continue; }
    if (espaco) { limpo += ' '; espaco = false; }
    limpo += c;
  }
  limpo = limpo.trim();
  return limpo.length > max ? limpo.slice(0, max - 1) + '…' : limpo;
}

/** O texto da última mensagem que não é minha, para o aviso — só quando foi pedido. */
async function ultimoTexto(servidor, canal) {
  const msgs = await invoke('mensagens', { servidor, canal }).catch(() => []);
  for (let i = msgs.length - 1; i >= 0; i--) {
    if (msgs[i].autor !== vista.chave) return msgs[i].texto;
  }
  return null;
}

// `null` e não um Map vazio, e a diferença importa: com um Map vazio, a primeira comparação
// depois de arrancar via TODO o não lido como «subiu» e despejava um aviso por cada canal
// com atraso. `null` quer dizer «ainda não sei o que havia» — a primeira foto só regista.
let porLerAnterior = null;

/** Avisa do que subiu desde a ultima vez, e so com a janela fora da frente.
 *
 * Comparar com a foto anterior em vez de reagir ao evento e o que evita avisar duas vezes
 * pela mesma mensagem: o `servidor-mudou` dispara tambem quando sou EU a escrever, quando
 * chega historico antigo, e varias vezes durante um sync.
 */
/** Regista o que ja estava por ler, sem avisar de nada.
 *
 *  Tem de ser chamado no ARRANQUE. Estava a ser feito dentro do `talvezAvisar`, que so corre
 *  no `servidor-mudou` — portanto a fotografia de base era tirada na PRIMEIRA mensagem que
 *  chegasse, e essa nunca avisava. Numa app de mensagens, a primeira mensagem de cada sessao
 *  e exactamente aquela de que se quer saber.
 */
function fotografarPorLer() {
  porLerAnterior = fotoDoPorLer(vista);
}

async function talvezAvisar() {
  const agora = fotoDoPorLer(vista);
  if (porLerAnterior === null) { porLerAnterior = agora; return; }
  // ESCREVER A FOTOGRAFIA PRIMEIRO.
  //
  // Entre ler o `porLerAnterior` e escrevê-lo havia dois `await` (ir buscar o texto, mandar
  // o aviso) que devolvem o controlo ao ciclo de eventos. O `listen` do Tauri não serializa
  // nada: uma segunda mensagem a chegar no meio entrava aqui com o mapa AINDA por
  // actualizar, via a mesma subida outra vez, e avisava duas vezes pela mesma mensagem.
  const anterior = porLerAnterior;
  porLerAnterior = agora;

  const focada = janelaComFoco;
  for (const [k, v] of agora) {
    const antes = anterior.get(k);
    if (v.n <= (antes ? antes.n : 0) || focada) continue;
    const t = avisosComTexto() ? await ultimoTexto(v.servidor, v.canal) : null;
    await avisar(umaLinha(v.quem, 40), corpoDoAviso(v.onde, t));
  }
}

listen('servidor-mudou', async ev => {
  await desenharTudo();
  await talvezAvisar();
  // O chat da sala vive na coluna da direita, fora da vista de canal: se estivermos a
  // ler um canal de texto, o desenharTudo não lhe toca e as mensagens novas não apareciam.
  await desenharChatDaSala();
});
/** A versão de cada par, por chave. Vazio até ele dizer.
 *
 *  Existe para a degradação deixar de ser muda (#4): quando chega uma mensagem que esta
 *  versão não conhece, ignora-se e segue-se — o que é certo para a ligação e errado para a
 *  pessoa, que vê uma funcionalidade a não funcionar sem saber porquê. */
const versaoDoPar = new Map();
let minhaVersao = null;

listen('peer-versao', ev => {
  const [chave, dele, minha] = ev.payload || [];
  if (!chave) return;
  minhaVersao = minha;
  versaoDoPar.set(chave, dele);
  desenharTudo();
});

/** A etiqueta de versão de um par, ou null se estiver igual à minha (aí não há que dizer). */
function avisoDeVersao(chave) {
  const dele = versaoDoPar.get(chave);
  if (!dele || !minhaVersao || dele === minhaVersao) return null;
  return `tem a ${dele}, tu tens a ${minhaVersao}`;
}

listen('peer-ligado', () => { ligados += 1; desenharTopo(); });
listen('peer-desligado', () => { ligados = Math.max(0, ligados - 1); desenharTopo(); });

/* ---------- explicações: o porquê vive na app ---------- */

const EXPLICACOES = {
  identidade: {
    titulo: 'A tua identidade',
    corpo: [
      'Foi criada neste computador na primeira vez que abriste a app. É uma chave, e é ao mesmo tempo o teu ID e o teu endereço na rede.',
      'Não existe conta nem registo. Mas existem <b>24 palavras</b> que a recuperam noutra máquina — se as guardares antes de precisares delas.',
    ],
    accao: { rotulo: 'Ver as minhas 24 palavras', abre: 'veu-definicoes' },
  },
  e2ee: {
    titulo: 'Cifrado ponta a ponta',
    corpo: [
      'As mensagens são cifradas <b>antes</b> de saírem deste computador, com uma chave que só os membros do servidor têm.',
      'O que <b>não</b> esconde: quem fala com quem e quando. Isso chama-se metadados.',
    ],
  },
  caminho: {
    titulo: 'Quem está ligado',
    corpo: [
      'Não há servidor. Isto conta quantos membros estão ligados a ti <b>neste momento</b>, diretamente.',
      'É com eles que o teu histórico sincroniza. Se não houver ninguém ligado, nada de novo chega — e nada do que escreveres sai daqui até alguém aparecer.',
    ],
  },
  historico: {
    titulo: 'Porque é que quem está online importa',
    corpo: [
      'O histórico deste servidor existe nos computadores dos membros, e mais em lado nenhum.',
      '<b>Se ninguém do servidor estiver online, não há nada de onde puxar.</b> É o preço direto de não haver uma máquina no meio.',
    ],
  },
  'chat-voz': {
    titulo: 'O chat desta sala',
    corpo: [
      'É um canal à parte dos canais de texto, e só aparece enquanto estiveres na sala. O histórico fica: sais, voltas, e continua lá.',
      '<b>Esconder não é o mesmo que cifrar.</b> Isto é uma regra desta app: a mensagem viaja com a chave do servidor, igual a todas as outras, por isso chega ao computador de todos os membros. Um cliente modificado conseguia lê-la sem entrar na sala.',
      'Para ser garantia a sério, a sala precisava de chave própria — e ainda não tem.',
    ],
  },
  expulsar: {
    titulo: 'Membros e chaves',
    corpo: [
      'Quem aparece aqui é quem já escreveu alguma coisa neste servidor. A identidade vem da assinatura de cada entrada, não de um registo.',
      'O <b>convite contém a chave do servidor</b>: quem o tiver consegue ler tudo o que for escrito a partir do momento em que entra. Trata-o como um segredo.',
    ],
  },
};

const painelExplica = $('#explica');
function mostrarExplicacao(chave, ancora) {
  const e = EXPLICACOES[chave];
  if (!e) return;
  $('#explica-titulo').textContent = e.titulo;
  const corpo = $('#explica-corpo');
  corpo.textContent = '';
  for (const p of e.corpo) {
    const el = document.createElement('p');
    el.innerHTML = p;   // literais desta constante, nunca dados de fora
    corpo.append(el);
  }
  if (e.accao) {
    const b = elemento('button', 'btn btn--primary', e.accao.rotulo);
    b.onclick = () => { esconderExplicacao(); abrirDefinicoes(); };
    corpo.append(b);
  }
  painelExplica.hidden = false;
  const r = ancora.getBoundingClientRect();
  const largura = painelExplica.offsetWidth;
  let x = Math.max(12, Math.min(r.left + r.width / 2 - largura / 2, innerWidth - largura - 12));
  let y = r.bottom + 8;
  if (y + painelExplica.offsetHeight > innerHeight - 12) {
    y = Math.max(12, r.top - painelExplica.offsetHeight - 8);
  }
  painelExplica.style.left = `${Math.round(x)}px`;
  painelExplica.style.top = `${Math.round(y)}px`;
}
const esconderExplicacao = () => { painelExplica.hidden = true; };

document.addEventListener('click', ev => {
  const gatilho = ev.target.closest('[data-explica]');
  if (gatilho) {
    ev.stopPropagation();
    if (!painelExplica.hidden && painelExplica.dataset.chave === gatilho.dataset.explica) {
      return esconderExplicacao();
    }
    painelExplica.dataset.chave = gatilho.dataset.explica;
    return mostrarExplicacao(gatilho.dataset.explica, gatilho);
  }
  if (!ev.target.closest('#explica')) esconderExplicacao();
});
document.addEventListener('keydown', ev => {
  if (ev.key === 'Escape') esconderExplicacao();
});

/* A névoa é um blur de ecrã inteiro: não gastar GPU com a janela escondida. */
document.addEventListener('visibilitychange', () => {
  const fog = $('.fog');
  if (fog) fog.style.animationPlayState = document.hidden ? 'paused' : 'running';
});

/* --------------------------------------------------------------------------
   Atualizações.

   O plugin sozinho não faz nada — é preciso alguém perguntar. E a atualização
   nunca se instala em silêncio: quem está a usar a app decide quando reinicia,
   porque reiniciar a meio de uma conversa é uma coisa que se faz a alguém.
   -------------------------------------------------------------------------- */

/** Procura uma versão nova.
 *
 *  Devolve `'ha'`, `'nao'` ou `'falhou'` -- três respostas e não duas, porque «não há
 *  versão nova» e «não consegui saber» são coisas diferentes para quem carregou no botão.
 *  Enquanto isto devolvia `false` nos dois casos, as Definições diziam "já estás na versão
 *  mais recente" a alguém que estava sem rede. */
/* A versão que a pessoa adiou NESTA sessão. As procuras automáticas respeitam-na — quem
   disse «agora não» não quer a mesma faixa de quatro em quatro horas —, mas o botão das
   Definições ignora-a: quem pergunta quer resposta. Uma versão MAIS nova volta a aparecer,
   porque já não é a que se adiou. */
let versaoAdiada = null;

async function procurarAtualizacao(automatica = false) {
  try {
    const { check } = window.__TAURI__.updater;
    const nova = await check();
    if (!nova) return 'nao';
    if (automatica && nova.version === versaoAdiada) return 'ha';
    // A primeira linha das notas é o assunto da versão — «uma sala grande volta a poder
    // sincronizar» — e é o que permite decidir se vale a pena reiniciar já (#62). Antes
    // dizia só o número, e ninguém decide nada com um número. O resto fica no tooltip.
    // A primeira linha da anotação começa por «Bruma vX.Y.Z --», e a frase à volta já diz
    // as duas coisas: apara-se, senão a faixa gaguejava o nome e o número.
    const primeira = (nova.body || '').split('\n').map(l => l.trim()).find(l => l) || '';
    const resumo = primeira.replace(/^Bruma\s+v?[\d.]+\s*(--|—|-)\s*/i, '');
    $('#texto-update').textContent = resumo
      ? `Há uma versão nova do Bruma (${nova.version}): ${resumo}`
      : `Há uma versão nova do Bruma (${nova.version}).`;
    $('#texto-update').title = nova.body || '';
    $('#faixa-update').hidden = false;
    $('#adiar-update').onclick = () => {
      versaoAdiada = nova.version;
      $('#faixa-update').hidden = true;
    };
    $('#btn-update').onclick = async () => {
      $('#btn-update').disabled = true;
      $('#texto-update').textContent = 'A descarregar…';
      try {
        // A ultima linha que a app escreve antes de se fechar sozinha. Se o registo acabar
        // aqui, a instalacao nao chegou ao fim -- e o instalador continua a escrever nele.
        await invoke('capacidades', { linha: 'a instalar a actualizacao...' }).catch(() => {});
        await nova.downloadAndInstall();
        // Código morto, e fica escrito porquê: o `downloadAndInstall` acima chama
        // `exit(0)` assim que lança o instalador, portanto esta linha nunca corre. O que
        // relança a app é o próprio instalador, no fim.
        await window.__TAURI__.process.relaunch();
      } catch (e) {
        $('#texto-update').textContent = `Não consegui atualizar: ${e}`;
        $('#btn-update').disabled = false;
      }
    };
    return 'ha';
  } catch (e) {
    // Sem rede, ou o endpoint em baixo. No arranque não vale a pena incomodar ninguém —
    // por isso é que isto não atira. Mas quem carregou no botão tem de saber a diferença.
    console.warn('verificação de atualização falhou:', e);
    return 'falhou';
  }
}

/* ==========================================================================
   Menu de contexto próprio.

   O menu do WebView2 oferece "Guardar como", "Imprimir", "Enviar a guia para os
   teus dispositivos" e "Inspecionar" — vocabulário de browser, não de aplicação.
   Suprime-se e põe-se um que fale das coisas que existem aqui.
   ========================================================================== */

const menu = $('#menu');

function abrirMenu(x, y, itens) {
  menu.textContent = '';
  for (const it of itens) {
    if (it === '-') { menu.append(document.createElement('hr')); continue; }
    // O DESLIZADOR DO VOLUME (#164). É o único item do menu que não é um botão, e por
    // isso não fecha o menu ao ser mexido: quem está a acertar um volume quer ouvir o
    // resultado enquanto mexe.
    if (it.tipo === 'volume') {
      const linha = elemento('div', 'menu__vol');
      const r = document.createElement('input');
      r.type = 'range'; r.min = '0'; r.max = '200'; r.step = '5';
      r.value = String(Math.round(volumeDe(it.chave) * 100));
      const n = elemento('i', null, `${r.value}%`);
      r.oninput = () => {
        n.textContent = `${r.value}%`;
        guardarVolume(it.chave, Number(r.value) / 100);
      };
      linha.append(elemento('b', null, 'Volume'), r, n);
      // O `click` global que fecha o menu está em fase de CAPTURA e sem filtro de alvo, e o
      // `mouseup` de um arrasto produz um `click`. Sem isto, o comentário acima — «não fecha
      // o menu ao ser mexido» — era uma afirmação sobre código que fazia o contrário.
      linha.addEventListener('click', ev => ev.stopPropagation(), true);
      linha.addEventListener('mousedown', ev => ev.stopPropagation(), true);
      menu.append(linha);
      continue;
    }
    const b = elemento('button', it.perigo ? 'perigo' : null, it.rotulo);
    b.onclick = () => { menu.hidden = true; it.accao(); };
    menu.append(b);
  }
  menu.hidden = false;
  // Encostar ao rato, mas nunca sair do ecrã.
  const l = Math.min(x, innerWidth - menu.offsetWidth - 8);
  const t = Math.min(y, innerHeight - menu.offsetHeight - 8);
  menu.style.left = `${Math.max(8, l)}px`;
  menu.style.top = `${Math.max(8, t)}px`;
}

document.addEventListener('contextmenu', ev => {
  ev.preventDefault();          // <- é isto que mata o menu do browser
  const itens = [];

  const msg = ev.target.closest('.msg');
  const canal = ev.target.closest('.chan');
  const membro = ev.target.closest('.member');
  const seleccao = String(getSelection()).trim();

  if (seleccao) {
    itens.push({ rotulo: 'Copiar', accao: () => navigator.clipboard.writeText(seleccao) });
  }
  if (msg && !seleccao) {
    const p = msg.querySelector('p');
    const texto = p ? p.textContent : '';
    itens.push({ rotulo: 'Copiar mensagem', accao: () => navigator.clipboard.writeText(texto) });
  }
  if (membro && membro.dataset.chave) {
    const chave = membro.dataset.chave;
    itens.push({ rotulo: 'Copiar chave', accao: () => navigator.clipboard.writeText(chave) });
    // O volume só faz sentido de quem se está a ouvir agora (#164).
    if (chave !== vista.chave && voz.audio.has(chave)) {
      itens.push({ tipo: 'volume', chave });
    }
    // Uma linha, e serve os três sítios que mostram pessoas — a lista de membros, quem está
    // na chamada e as fotinhas — porque todos põem a chave no `data-chave`.
    if (chave !== vista.chave) {
      itens.push({
        rotulo: 'Adicionar aos amigos',
        accao: async () => {
          const nome = prompt('Que nome lhe queres dar?', nomeDoPeer(chave));
          if (!nome) return;
          try {
            await invoke('adicionar_amigo', { chave, nome });
            await desenharTudo();
          } catch (e) { alert(String(e)); }
        },
      });
      itens.push({
        rotulo: 'Mensagem privada',
        accao: async () => {
          try {
            const id = await invoke('abrir_conversa', { peer: chave });
            await desenharTudo();
            escolherConversa(id);
          } catch (e) {
            // Falta a chave de conversa dele: ainda não estiveram ligados desde que os dois
            // actualizaram. Dizer porquê vale mais do que não acontecer nada.
            alert(String(e));
          }
        },
      });
    }
  }
  if (canal && canal.dataset.canal) {
    const id = canal.dataset.canal;
    if (itens.length) itens.push('-');
    itens.push({
      rotulo: 'Apagar canal', perigo: true,
      accao: () => invoke('apagar_canal', { servidor: servidorAtual, canal: id }).catch(console.error),
    });
  }
  // Só no modo servidor: no modo privado o `servidorAtual` continua preenchido por baixo, e
  // isto oferecia "convidar alguém" para um servidor que não está no ecrã — e o que sairia
  // seria a chave que o decifra.
  if (modo === 'servidor' && servidorAtual && !canal && !msg && !membro) {
    itens.push({ rotulo: 'Convidar alguém', accao: () => $('#btn-convite').click() });
  }
  if (itens.length) itens.push('-');
  itens.push({ rotulo: 'Como isto se liga…', accao: abrirDefinicoesDeRede });

  abrirMenu(ev.clientX, ev.clientY, itens);
});

document.addEventListener('click', () => { menu.hidden = true; }, true);
document.addEventListener('keydown', ev => { if (ev.key === 'Escape') menu.hidden = true; });

/* ==========================================================================
   Voz e partilha de ecrã.

   A sinalização vai por cima do iroh, que já resolveu o NAT para o chat. O WebRTC
   faz o seu próprio caminho para a média, e por isso pode precisar de TURN — daí
   as definições de ligação.
   ========================================================================== */

const voz = {
  eu: null,
  servidor: null,
  canal: null,
  micro: null,
  ecra: null,
  camara: null,
  audio: new Map(),      // peer -> como se lhe ouve a voz
  presentes: new Map(),  // peer -> canal em que está
  falando: new Set(),    // quem está a emitir som agora
  silenciados: new Set(),// pessoas silenciadas uma a uma
  aPartilhar: new Set(), // quem está a transmitir o ecrã
  comCamara: new Set(),  // quem tem a câmara ligada
  /** O que cada transmissor diz sobre a transmissão dele: qualidade e quantos o veem. */
  infoDaTransmissao: new Map(),
  /** O tamanho com que a MINHA captura ficou mesmo, devolvido pelo Rust. */
  ecraTamanho: null,
  /** A qualidade que está MESMO a correr, congelada quando a partilha começou.
   *
   *  Sem isto, o rótulo lia o menu no momento de desenhar — e quem mexesse na engrenagem a
   *  meio via a barra a anunciar números que ninguém estava a usar. Mudar a qualidade só
   *  vale para transmissões novas, e a barra tem de contar a mesma história. */
  qualidadeEmUso: null,
  // Quem, do outro lado, percebe o que esta versão envia. Ver PROTOCOLO.
  entendeCamara: new Set(),
  entendeSom: new Set(),
  // De quem já recebemos um anúncio de estado. Sem isto não se distingue "é antigo" de
  // "ainda não disse nada", e as duas coisas mereciam respostas opostas.
  jaFalou: new Set(),
  aVer: null,            // de quem estou a ver a transmissão
  aSerVistoPor: new Set(), // quem pediu para ver o MEU ecrã — só a esses se envia
  vejoMeuEcra: null,       // o <video> da minha própria transmissão, só enquanto olho
  analisadores: new Map(),
  audioCtx: null,
};

/** Já não há definições de rede — este painel passou a explicar porque é que não há, e a
 *  mostrar o que está mesmo a acontecer.
 *
 *  O que está aqui é o que transforma um "não se ouve nada" numa resposta: se saíram
 *  pacotes e não entrou nenhum, o problema é do outro lado; se não saiu nenhum, é deste;
 *  se entraram e saíram e mesmo assim não se ouve, o problema não é a rede. São três
 *  sítios diferentes, e sem isto escolhe-se um à sorte.
 */
function abrirDefinicoesDeRede() {
  abrir('veu-rede');
  desenharDiagnostico();
}

let relogioDiag = null;
async function desenharDiagnostico() {
  const alvo = $('#diag-rede');
  if (!alvo) return;
  if ($('#veu-rede').hidden) {
    if (relogioDiag) { clearInterval(relogioDiag); relogioDiag = null; }
    return;
  }
  if (!relogioDiag) relogioDiag = setInterval(desenharDiagnostico, 1500);

  const gente = [...voz.presentes.keys()];
  if (!gente.length) {
    alvo.textContent = 'Ninguém ligado neste momento.';
    return;
  }
  const estado = await invoke('qualidade', { peers: gente }).catch(() => []);
  alvo.textContent = '';
  if (!estado.length) {
    alvo.textContent = `${gente.length} presente(s), nenhuma ligação aberta ainda.`;
    return;
  }
  for (const e of estado) {
    const linha = elemento('div', 'diag__linha');
    linha.append(elemento('span', 'diag__quem', nomeDoPeer(e.peer)));
    const caminho = e.relay ? 'por relay' : 'direta';
    // `null` é «ninguém mediu», e é diferente de zero (#171).
    // «<1 ms» e não «0 ms»: um RTT medido que arredonda a zero escrevia exactamente o mesmo
    // que um RTT inexistente, que é a confusão que o #171 existe para desfazer.
    const ms = typeof e.ms === 'number' && e.ms > 0
      ? ` · ${e.ms < 0.5 ? '<1' : Math.round(e.ms)} ms`
      : ' · RTT por medir';
    // O ACUMULADO E O AGORA (#33). Os totais só crescem: com «↑30000 ↓29000» o painel
    // parecia saudável para sempre, mesmo que a voz dela tivesse morrido ao minuto dez. Os
    // pacotes por segundo e o «há quanto tempo» dizem o presente.
    const voz_ = `voz ↑${e.enviados} ↓${e.recebidos} (${e.envS}/s ↑, ${e.recS}/s ↓)`;
    const mudoDesdeSempre = e.recebidos === 0 && e.enviados > 0;
    const calouSeAgora = e.recebidos > 0 && e.recS === 0
      && typeof e.haQuantoRec === 'number' && e.haQuantoRec > 3000;
    const recusado = e.vozFalhados > 0 ? ` · ${e.vozFalhados} recusados pelo transporte` : '';
    // A PERDA (#124). Até aqui era impossível: o receptor não tinha como distinguir «ele
    // calou-se» de «perdi trinta pacotes». Agora o outro lado diz quantos mandou.
    const perda = typeof e.perda === 'number'
      ? ` · ${e.perda.toFixed(1)}% perdidos` : ' · perda por medir';
    const desde = calouSeAgora ? ` · sem som há ${Math.round(e.haQuantoRec / 1000)} s` : '';
    // OS CORTES, QUE ATÉ AQUI NÃO DEIXAVAM RASTO NENHUM (#65). «recomeçou 14 vezes no
    // último minuto» é o género de frase que transforma «está mau» em algo com sítio para
    // procurar — e a folga em uso ao lado diz o que a adaptação escolheu (#104, #117).
    const c = cortesDaVoz.get(e.peer);
    const cortes = c && c.total > 0
      ? ` · som recomeçado ${cortesDesdeHa(c, 60000).length}x no último minuto`
        + ` (${c.total} nesta chamada,`
        + ` ${c.saltado.toFixed(1)} s saltados) · folga ${Math.round(c.folga * 1000)} ms`
      : '';
    const d = elemento('span', (mudoDesdeSempre || calouSeAgora) ? 'diag__mudo' : null,
      `${caminho}${ms} · ${voz_}${perda}${recusado}${desde}${cortes}`);
    linha.append(d);
    alvo.append(linha);
  }
}
$('#fechar-rede').onclick = () => fechar('veu-rede');

async function entrarEmVoz(servidor, canal) {
  vozFalhou = null;
  vozPartida.clear();
  // O PICO E A SAÍDA SÃO DESTA CHAMADA, e não da sessão (#36, #106).
  //
  // Sem isto, quem saísse de uma chamada depois de uns minutos calado voltava a entrar com
  // o relógio do chão já com mais de 15 segundos da chamada anterior — e a marca «o
  // teu microfone está aberto mas não capta nada há 15 s» aparecia no PRIMEIRO instante da
  // chamada nova, antes de se ter medido o que quer que fosse. Uma acusação sobre um passado
  // que já não é o desta chamada.
  // O relógio arranca AGORA: os quinze segundos contam-se desde que a chamada abriu, e
  // não desde um instante de outra chamada.
  acimaDoChaoEm = performance.now();
  ultimoPedacoSaiu = 0;
  // E OS CORTES TAMBÉM (#65, #104). O `cortesDaVoz` sobrevive de propósito ao `calarPeer`,
  // para uma pessoa que sai e volta a entrar na sala não zerar a contagem a meio da
  // chamada. Mas sobreviver à CHAMADA é outra coisa: o painel escrevia «N na chamada» sobre
  // o total desde o arranque da app, e — pior — a `folga` viajava. Uma chamada má subia-a
  // para 200 ms e a seguinte, com rede boa, arrancava aos 200 e levava seis minutos limpos
  // a voltar aos 80. É o walkie-talkie contra o qual o próprio tecto foi escrito.
  cortesDaVoz.clear();
  if (voz.canal === canal) return;
  await sairDeVoz(false);
  voz.servidor = servidor;
  voz.canal = canal;
  // Desenhar JA, antes de pedir o microfone. O pedido de autorizacao pode ficar minutos
  // a espera de resposta -- ou nunca ser respondido -- e ate la a app parecia presa a
  // dizer "nao estas nesta sala" quando ja estava.
  desenharVoz();
  desenharCanais();
  await invoke('presenca_de_voz', { servidor, canal }).catch(console.error);

  try {
    // Com limite de tempo: se ninguem responder ao pedido, entra-se sem microfone em vez
    // de ficar pendurado para sempre.
    voz.micro = await Promise.race([
      abrirMicrofone(),
      new Promise((_, rej) => setTimeout(() => rej(new Error('sem resposta ao pedido')), 20000)),
    ]);
    if (voz.canal !== canal) {          // saiu enquanto se esperava
      voz.micro.getTracks().forEach(t => t.stop());
      voz.micro = null;
      return;
    }
    // ENTRAR SURDO NÃO PODE DEIXAR O MICROFONE A TRANSMITIR. Este defeito é anterior à
    // fase 5 — o `$('#btn-surdo').onclick` só põe `t.enabled = false` no instante do
    // clique, e uma chamada aberta DEPOIS disso trazia uma faixa nova a `true` — mas é a
    // mesma avaria que a reabertura tinha, e corrigir uma e deixar a outra seria escolher
    // qual das duas vezes é que o botão mente.
    const faixaNova = voz.micro.getAudioTracks()[0];
    if (faixaNova && surdo) faixaNova.enabled = false;
    comecarAEnviarVoz(voz.micro);
    vigiarAudio(voz.eu, voz.micro);
    // O que o dispositivo diz que está a fazer, mal ele abra (#35, #191): o painel desenhava
    // o PEDIDO, e um `getUserMedia` pode ignorar metade do que se lhe pediu sem falhar.
    lerRuidoReal();
    desenharVoz();
  } catch (e) {
    // Sem microfone continua a dar para ouvir e para partilhar ecra.
    console.warn('sem microfone:', e);
    // Aparecer mudo SEM RAZÃO é o pior dos dois mundos: a pessoa vê o ícone cortado e
    // assume que carregou nele sem querer.
    vozFalhou = `Sem microfone: ${e && e.message ? e.message : e}`;
    voz.micro = null;
  }
  desenharVoz();
}

async function sairDeVoz(anunciar = true) {
  // A câmara desliga-se ao sair, sempre. Deixar a luz acesa depois de sair da chamada era
  // a pior falha de confiança que esta app podia ter.
  pararDeEnviarCamara();
  for (const chave of [...camarasRecebidas.keys()]) fecharCamaraRecebida(chave);
  if (anunciar && voz.canal) {
    await invoke('presenca_de_voz', { servidor: voz.servidor, canal: null }).catch(() => {});
  }
  pararDeEnviarVoz();
  for (const chave of [...voz.audio.keys()]) calarPeer(chave);
  if (voz.micro) voz.micro.getTracks().forEach(t => t.stop());
  if (voz.ecra) invoke('parar_de_partilhar').catch(() => {});
  if (voz.vejoMeuEcra) { voz.vejoMeuEcra.fechar(); voz.vejoMeuEcra = null; }
  for (const chave of [...fluxosRecebidos.keys()]) fecharFluxoRecebido(chave);
  for (const chave of [...voz.analisadores.keys()]) pararDeVigiar(chave);
  voz.falando.clear();
  voz.aPartilhar.clear();
  // Quem estava a assistir deixou de estar: sair da chamada acaba com todos os pedidos.
  // Sem isto, voltar a entrar no mesmo canal trazia os espectadores antigos de volta —
  // o olho dizia "1" com ninguém a ver, e à primeira actualização da lista o ecrã
  // voltava a ser ENVIADO a quem já não tinha pedido nada. Uma cópia inteira de upload
  // para alguém que nem a ia mostrar.
  voz.aSerVistoPor.clear();
  voz.comCamara.clear();
  voz.infoDaTransmissao.clear();
  voz.entendeCamara.clear();
  voz.entendeSom.clear();
  voz.jaFalou.clear();
  voz.aVer = null;
  voz.micro = null; voz.ecra = null; voz.ecraTamanho = null; voz.qualidadeEmUso = null; voz.qualidadeEmUso = null;
  voz.canal = null;
  // O CONTEXTO DE SAÍDA FECHA-SE E ESQUECE-SE (#38).
  //
  // O vigia do relógio diz «sai da chamada e volta a entrar» quando o `currentTime` pára.
  // Sem estas três linhas o conselho era falso: o `vozCtx` era criado uma vez e nunca mais
  // voltava a `null`, portanto voltar a entrar reencontrava o MESMO contexto parado e o
  // `resume()` do `contextoDeAudio` era o mesmo que o vigia já tinha tentado sem sucesso.
  // Agora a chamada seguinte constrói um contexto novo, que é o que a frase promete.
  if (vozCtx) {
    const velho = vozCtx;
    vozCtx = null;
    try { velho.close(); } catch (e) { /* já fechado */ }
  }
  saidaFalhou = null;
  ultimoRelogioDaVoz = -1;
  voltasParado = 0;
  // O que o microfone dizia estar a fazer morreu com ele (#191).
  ruidoReal = null;
  desenharVoz();
  desenharRodape();
}

function sinalizar(peer, dados) {
  invoke('enviar_sinal', {
    para: peer, servidor: voz.servidor, canal: voz.canal, dados: JSON.stringify(dados),
  }).catch(console.error);
}

/** Os avisos que trocamos entre nós: quem está a transmitir e quem está a ver.
 *
 *  Já não há aqui SDP nem candidatos ICE. A voz e o ecrã vão os dois pelo iroh, que trata
 *  do NAT sozinho — o que desapareceu com o WebRTC foi a negociação toda e, com ela, a
 *  necessidade de configurar servidores de ligação à mão.
 */
async function receberSinal(de, dados) {
  if (dados.tipo === 'assistir') {
    if (dados.ligado) voz.aSerVistoPor.add(de); else voz.aSerVistoPor.delete(de);
    actualizarEspectadores();
    return;
  }
  if (dados.tipo === 'estado') {
    const versao = Number.isFinite(dados.v) ? dados.v : 1;
    voz.jaFalou.add(de);
    if (versao >= 2) {
      voz.entendeCamara.add(de);
      voz.entendeSom.add(de);
    } else {
      voz.entendeCamara.delete(de);
      voz.entendeSom.delete(de);
    }
    if (dados.ecra) {
      voz.aPartilhar.add(de);
    } else {
      voz.aPartilhar.delete(de);
      fecharFluxoRecebido(de);
    }
    if (dados.ecra) {
      voz.infoDaTransmissao.set(de, {
        qualidade: typeof dados.qualidade === 'string' ? dados.qualidade : null,
        espectadores: Number.isFinite(dados.espectadores) ? dados.espectadores : 0,
      });
    } else {
      voz.infoDaTransmissao.delete(de);
    }
    if (dados.camara) {
      voz.comCamara.add(de);
    } else {
      voz.comCamara.delete(de);
      fecharCamaraRecebida(de);
    }
    if (voz.aVer === de && !dados.ecra) voz.aVer = null;
    desenharVoz();
  }
}

/* ==========================================================================
   Partilha de ecrã: captada e codificada em Rust, não pela webview.

   O `getDisplayMedia` funcionava, mas trazia duas coisas que não se resolvem por
   configuração: o WebView2 desenhava por cima da app a barra "está a partilhar uma
   janela" — não há API nem flag para a tirar, porque é o indicador de segurança dele — e
   o codificador acabava por ser software, com a placa parada ao lado.

   Agora o Rust capta, codifica com o codificador da placa, e manda pedaços de MP4
   fragmentado. Aqui só se juntam os pedaços e se entregam a um `<video>` pelo
   MediaSource, que é o que o navegador sabe fazer sem ajuda.
   ========================================================================== */

/* As etiquetas com que o Rust marca cada pedaço: bytes de vídeo, ou o nome do codec. */
const ETIQUETA_BYTES = 0;
const ETIQUETA_CODEC = 1;

/** Um `<video>` alimentado aos pedaços.
 *
 *  O MediaSource não aceita bytes enquanto está ocupado a digerir os anteriores, e o
 *  `appendBuffer` atira exceção se lho fizerem. Por isso há fila: os pedaços chegam ao
 *  ritmo do codificador, não ao ritmo a que o navegador os quer.
 */
function fluxoDePedacos(comSom = false) {
  const media = new MediaSource();
  const el = document.createElement('video');
  el.autoplay = true;
  el.playsInline = true;
  // Nasce mudo SEMPRE, porque um <video> que nasce com som pode nem chegar a arrancar: a
  // política de autoplay só deixa passar o que não faz barulho sem alguém ter mandado.
  //
  // E depois, se for o ecrã de outra pessoa, desmuta-se — mas só depois de estar mesmo a
  // tocar, que é quando desmutar já não o impede de arrancar.
  //
  // Isto estava só a primeira metade. O som do sistema era captado (o `som.rs` inteiro,
  // com o loopback de processo para não haver eco), viajava no mesmo fragmento que a
  // imagem, chegava ao outro lado, era descodificado -- e ia dar a um elemento mudo.
  // NINGUÉM o ouviu nunca. Não deu nas vistas porque as duas instâncias do teste correm
  // na mesma máquina, e aí o som sai das colunas de qualquer maneira.
  el.muted = true;
  if (comSom) {
    el.addEventListener('playing', () => { el.muted = false; }, { once: true });
  }
  el.src = URL.createObjectURL(media);

  const fila = [];
  el.__aparados = 0;
  el.__filaMax = 0;
  el.__pedacos = 0;
  let buffer = null;
  let codec = null;
  let aberto = false;
  // Porque é que este fluxo não dá imagem, se não der. O comentário aqui ao lado já avisava
  // que o codec VARIA com a placa gráfica de quem partilha — ou seja, isto vai acontecer em
  // máquinas reais, e ia parecer um problema de ligação.
  let recusa = null;

  /* O codec não se assume, vem escrito no fluxo.
     O `addSourceBuffer` obriga a declará-lo, e o navegador VALIDA o que se lhe declara
     contra o cabeçalho: se não bater certo, recusa tudo com um "stream parsing failed"
     que não explica nada. Nesta máquina a NVIDIA produz Baseline 4.2; noutra placa será
     outro. Por isso espera-se por ele antes de abrir o buffer. */
  const abrir = () => {
    if (buffer || !aberto || !codec) return;
    const tipo = `video/mp4; codecs="${codec}"`;
    el.__codec = codec;
    if (!window.MediaSource || !MediaSource.isTypeSupported(tipo)) {
      console.warn('esta webview não sabe descodificar', tipo);
      recusa = `Esta máquina não sabe descodificar ${tipo}. `
        + 'A placa gráfica de quem partilha produziu um formato que esta não lê.';
      desenharVoz();
      return;
    }
    try {
      buffer = media.addSourceBuffer(tipo);
      buffer.mode = 'sequence';
      buffer.addEventListener('updateend', escoar);
      escoar();
    } catch (e) {
      console.warn('não consegui abrir o buffer de vídeo:', e);
      recusa = `Não consegui abrir o vídeo: ${e && e.message ? e.message : e}`;
      desenharVoz();
    }
  };

  const escoar = () => {
    if (!buffer || buffer.updating || !fila.length) return;
    try {
      buffer.appendBuffer(fila.shift());
    } catch (e) {
      // QuotaExceeded: o buffer encheu. Deita-se fora o que já passou — numa transmissão
      // ao vivo ninguém quer rebobinar, e guardar tudo acabaria por rebentar a memória.
      if (e.name === 'QuotaExceededError' && el.buffered.length) {
        try { buffer.remove(0, Math.max(0, el.currentTime - 2)); } catch (_) { /* logo se vê */ }
      } else {
        console.warn('o vídeo recusou o pedaço:', e);
      }
    }
  };

  /** Mantém quem vê colado ao PRESENTE.
   *
   *  # A avaria que isto corrige
   *
   *  Um `<video>` reproduz a 1× e não sabe que isto é ao vivo. Quem carrega em "Assistir"
   *  entra pelo princípio do que já foi enviado, e a partir daí só perde terreno: chega
   *  mais um segundo de imagem por cada segundo que passa, e o atraso NUNCA encolhe. Foi
   *  medido: buffer com 25 segundos e o leitor em 6,62 — dezoito segundos atrás. Quem
   *  partilha mexe o rato e o outro só vê isso vinte segundos depois; do lado de lá parece
   *  que a partilha não está a funcionar, e no fundo está — só que no passado.
   *
   *  Nota-se mais num ecrã largo, porque gera mais dados e o descodificador perde terreno
   *  mais depressa. Foi por isso que apareceu ao partilhar o ultrawide e não o 16:9.
   *
   *  A cura é a de qualquer leitor ao vivo: quando o atraso passa de um limite, SALTA-SE
   *  para a frente. Salta-se em vez de acelerar porque um salto é honesto — perde-se o que
   *  se perdeu — e acelerar a imagem e o som soa mal e nunca chega a apanhar.
   */
  const FOLGA = 0.6;      // o quanto se fica atrás da ponta, para não secar a cada soluço
  const LIMITE = 2.0;     // acima disto salta-se; abaixo, deixa-se estar
  const perseguirOVivo = () => {
    if (!buffer || !el.buffered.length) return;
    const ponta = el.buffered.end(el.buffered.length - 1);
    const atraso = ponta - el.currentTime;
    if (atraso > LIMITE) {
      const destino = Math.max(0, ponta - FOLGA);
      // `fastSeek` não existe em todo o lado e é aproximado; a atribuição direta é exata.
      try { el.currentTime = destino; } catch (e) { /* o próximo tick tenta */ }
    }
    // E deita-se fora o passado, que ninguém vai rebobinar. Sem isto a memória do buffer
    // cresce durante toda a chamada, e é o mesmo que a levar a rebentar devagar.
    if (!buffer.updating && el.buffered.length) {
      const inicio = el.buffered.start(0);
      const corte = el.currentTime - 4;
      if (corte > inicio + 2) {
        try { buffer.remove(inicio, corte); } catch (e) { /* logo se vê */ }
      }
    }
    // Um `<video>` que ficou sem dados a meio fica em pausa e não volta sozinho.
    //
    // Mas uma pausa PEDIDA é outra coisa. Quem partilha e muda de janela tem a
    // pré-visualização pausada de propósito, para poupar recursos — e sem esta bandeira
    // este relógio desfazia-a meio segundo depois. Duas correções minhas a lutar uma com a
    // outra, e a que corre mais vezes ganhava.
    if (el.paused && el.readyState >= 2 && !el.__pausaPedida) el.play().catch(() => {});
  };
  const relogioDoVivo = setInterval(perseguirOVivo, 500);

  media.addEventListener('sourceopen', () => {
    aberto = true;
    abrir();
  }, { once: true });

  return {
    el,
    empurrar(marcado) {
      if (!marcado.length) return;
      const etiqueta = marcado[0];
      const bytes = marcado.subarray(1);
      if (etiqueta === ETIQUETA_CODEC) {
        codec = new TextDecoder().decode(bytes);
        abrir();
        return;
      }
      if (etiqueta !== ETIQUETA_BYTES) return;
      fila.push(bytes);
      el.__pedacos += 1;
      if (fila.length > el.__filaMax) el.__filaMax = fila.length;
      // Se a fila crescer é porque o navegador não acompanha; nesse caso o que interessa
      // é o presente, não o passado.
      if (fila.length > 60) {
        el.__aparados += fila.length - 30;
        fila.splice(0, fila.length - 30);
      }
      escoar();
    },
    /** A razão de não haver imagem, quando não há. `null` significa "ainda a chegar". */
    porqueNaoDa() { return recusa; },
    fechar() {
      clearInterval(relogioDoVivo);
      try { el.pause(); } catch (e) { /* já parado */ }
      try { URL.revokeObjectURL(el.src); } catch (e) { /* já libertado */ }
      el.removeAttribute('src');
      fila.length = 0;
    },
  };
}

/** Abre o seletor: um separador por tipo de fonte, como no Discord —
 *  Janelas num, Ecrãs noutro, cada um só com o seu. */
let fontesEmMemoria = [];
let abaActiva = 'janela';

function desenharFontes() {
  const lista = $('#lista-fontes');
  document.querySelectorAll('#abas-fontes .aba').forEach(a => {
    a.classList.toggle('is-activa', a.dataset.aba === abaActiva);
  });
  lista.textContent = '';
  const visiveis = fontesEmMemoria.filter(f => f.tipo === abaActiva);
  if (!visiveis.length) {
    lista.append(elemento('p', 'fontes__espera',
      abaActiva === 'ecra' ? 'não encontrei ecrãs' : 'não encontrei janelas para partilhar'));
    return;
  }
  for (const f of visiveis) {
    const cartao = elemento('button', 'fonte');
    if (f.miniatura) {
      const img = document.createElement('img');
      img.src = f.miniatura;
      cartao.append(img);
    }
    const nome = elemento('span', null, f.titulo);
    nome.title = f.titulo;
    cartao.append(nome);
    cartao.onclick = () => { fechar('veu-fontes'); iniciarPartilha(f.id); };
    lista.append(cartao);
  }
}

/** Quem, na sala, ainda não percebe o que esta versão envia.
 *
 *  Enquanto o `estado` do outro lado não chegar, ele não está em `entendeSom` — e não se
 *  avisa por isso: seria acusar toda a gente de estar desactualizada durante o primeiro
 *  segundo de cada chamada. Só conta quem já falou e disse ser antigo.
 */
function quemNaoVaiVer() {
  return [...voz.presentes.entries()]
    .filter(([p, c]) => c === voz.canal && voz.jaFalou.has(p) && !voz.entendeSom.has(p))
    .map(([p]) => p);
}

function desenharAvisoDeVersao() {
  const el = $('#aviso-versao');
  if (!el) return;
  const antigos = quemNaoVaiVer();
  const comSom = qualidadeDePartilha().som;
  if (!antigos.length) { el.hidden = true; return; }
  const nomes = antigos.map(nomeDoPeer).join(', ');
  el.hidden = false;
  // Duas mensagens diferentes porque as consequências são diferentes, e dizer "pode haver
  // problemas" às duas seria não dizer nada.
  el.textContent = comSom
    ? `${nomes} está numa versão antiga e NÃO vai ver esta partilha, porque ela leva o som `
      + `do sistema — a versão dele só sabe ler a imagem. Desliga o som na engrenagem, ou `
      + `pede-lhe para actualizar (a app faz isso sozinha ao reabrir).`
    : `${nomes} está numa versão antiga. A imagem vai ver; a câmara, não.`;
}

async function escolherFonte() {
  abrir('veu-fontes');
  desenharAvisoDeVersao();
  const lista = $('#lista-fontes');
  lista.innerHTML = '<p class="fontes__espera">a olhar para as janelas…</p>';
  fontesEmMemoria = await invoke('fontes_de_partilha').catch(() => []);
  if ($('#veu-fontes').hidden) return;   // cancelou antes de a lista chegar
  desenharFontes();
}

document.querySelectorAll('#abas-fontes .aba').forEach(a => {
  a.onclick = () => { abaActiva = a.dataset.aba; desenharFontes(); };
});
$('#fechar-fontes').onclick = () => fechar('veu-fontes');

/* --- o modo de transmissão, no menu da engrenagem -------------------------- */

/** A escolha guardada. Um MODO é um atalho: "jogos" e "texto" trazem os números feitos;
 *  "pers" usa os que a pessoa afinou. Mexer num número à mão muda para "pers" sozinho,
 *  como no Discord — escolher um valor É personalizar. */
const MODOS = {
  jogos: { altura: 1440, fps: 60 },
  texto: { altura: 0, fps: 15 },
};

function qualidadeDePartilha() {
  let q = {};
  try {
    q = JSON.parse(localStorage.getItem('bruma.qualidade') || '{}');
  } catch (e) { /* estraga-se, recomeça-se */ }
  return {
    modo: ['jogos', 'texto', 'pers'].includes(q.modo) ? q.modo : 'pers',
    altura: Number.isFinite(q.altura) ? q.altura : 1080,
    fps: [15, 30, 60].includes(q.fps) ? q.fps : 60,
    debito: Number.isFinite(q.debito) ? q.debito : 0,
    // Liga por omissão, como no Discord: quem partilha um jogo ou um vídeo quer que se
    // ouça, e quem não quer diz aqui — em vez de descobrir a meio que ninguém ouviu nada.
    som: q.som !== false,
  };
}

/** Os números que valem MESMO, resolvido o modo. O débito manual vale em qualquer modo. */
function qualidadeEfetiva() {
  const q = qualidadeDePartilha();
  const base = MODOS[q.modo] || { altura: q.altura, fps: q.fps };
  return { altura: base.altura, fps: base.fps, debito: q.debito, som: q.som };
}

$('#linha-som').onclick = () => guardarQualidade({ som: !qualidadeDePartilha().som });

function guardarQualidade(mudanca) {
  const q = { ...qualidadeDePartilha(), ...mudanca };
  localStorage.setItem('bruma.qualidade', JSON.stringify(q));
  desenharMenuTransmissao();
}

const OPCOES_ALTURA = [[720, '720p'], [1080, '1080p'], [1440, '1440p'], [0, 'Nativa']];
const OPCOES_FPS = [[15, '15'], [30, '30'], [60, '60']];
const OPCOES_DEBITO = [[0, 'automático'], [3_000_000, '3 Mbps'], [6_000_000, '6 Mbps'],
                       [10_000_000, '10 Mbps'], [16_000_000, '16 Mbps']];

function rotuloDe(opcoes, valor) {
  const par = opcoes.find(([v]) => v === valor);
  return par ? par[1] : String(valor);
}

function desenharMenuTransmissao() {
  const q = qualidadeDePartilha();
  const efetiva = qualidadeEfetiva();

  document.querySelectorAll('[data-modo]').forEach(l => {
    l.classList.toggle('is-activa', l.dataset.modo === q.modo);
  });
  $('#desc-pers').textContent =
    `${q.altura === 0 ? 'nativa' : q.altura + 'p'}, ${q.fps} ips`;
  $('#valor-altura').textContent = rotuloDe(OPCOES_ALTURA, efetiva.altura);
  $('#valor-fps').textContent = rotuloDe(OPCOES_FPS, efetiva.fps);
  $('#valor-debito').textContent = rotuloDe(OPCOES_DEBITO, q.debito);
  // A caixa diz SILENCIAR: marcada é som desligado. É a mesma leitura do Discord, e é o
  // contrário do que a variável guarda — daí a negação estar aqui, uma vez só.
  $('#mudo-transmissao').checked = !q.som;
  $('#desc-som').textContent = q.som
    ? 'o som das colunas segue com a imagem'
    : 'a transmissão vai muda';
  desenharAvisoDeVersao();

  const nome = q.modo === 'jogos' ? 'Jogos' : q.modo === 'texto' ? 'Texto' : null;
  $('#resumo-qualidade').textContent =
    `${nome ? nome + ' · ' : ''}${efetiva.altura === 0 ? 'Nativa' : efetiva.altura + 'p'}`
    + ` · ${efetiva.fps} ips${q.debito ? ' · ' + rotuloDe(OPCOES_DEBITO, q.debito) : ''}`;

  const subs = [
    ['#sub-altura', OPCOES_ALTURA, q.altura, v => guardarQualidade({ altura: v, modo: 'pers' })],
    ['#sub-fps', OPCOES_FPS, q.fps, v => guardarQualidade({ fps: v, modo: 'pers' })],
    ['#sub-debito', OPCOES_DEBITO, q.debito, v => guardarQualidade({ debito: v })],
  ];
  for (const [sel, opcoes, atual, aoEscolher] of subs) {
    const sub = $(sel);
    if (sub.hidden) continue;
    sub.textContent = '';
    for (const [valor, rotulo] of opcoes) {
      const b = elemento('button', 'menu-trans__opcao', rotulo);
      if (valor === atual) b.classList.add('is-activa');
      // Escolher fecha o submenu, como no Discord: a lista aberta depois da decisão é
      // só ruído entre a pessoa e o resto do menu.
      b.onclick = () => { sub.hidden = true; aoEscolher(valor); };
      sub.append(b);
    }
  }
}

$('#btn-qualidade').onclick = ev => {
  ev.stopPropagation();
  const m = $('#menu-transmissao');
  m.hidden = !m.hidden;
  $('#btn-qualidade').classList.toggle('is-on', !m.hidden);
  if (m.hidden) document.querySelectorAll('.menu-trans__sub').forEach(s => { s.hidden = true; });
  desenharMenuTransmissao();
};

document.querySelectorAll('[data-modo]').forEach(l => {
  l.onclick = () => guardarQualidade({ modo: l.dataset.modo });
});
document.querySelectorAll('[data-abre]').forEach(l => {
  l.onclick = () => {
    const sub = $('#sub-' + l.dataset.abre);
    sub.hidden = !sub.hidden;
    desenharMenuTransmissao();
  };
});
// Clicar fora fecha o menu, como qualquer menu decente.
document.addEventListener('mousedown', ev => {
  const m = $('#menu-transmissao');
  if (m.hidden) return;
  if (ev.target.closest('#menu-transmissao, #btn-qualidade')) return;
  m.hidden = true;
  $('#btn-qualidade').classList.remove('is-on');
  document.querySelectorAll('.menu-trans__sub').forEach(s => { s.hidden = true; });
});
desenharMenuTransmissao();

async function alternarEcra() {
  if (voz.ecra) {
    await invoke('parar_de_partilhar').catch(() => {});
    if (voz.vejoMeuEcra) { voz.vejoMeuEcra.fechar(); voz.vejoMeuEcra = null; }
    if (voz.aVer === voz.eu) voz.aVer = null;
    voz.ecra = null; voz.ecraTamanho = null; voz.qualidadeEmUso = null;
    voz.aSerVistoPor.clear();   // parar de transmitir acaba com os pedidos de assistir
    anunciarEstado();
    desenharVoz();
    desenharRodape();
    return;
  }
  if (!voz.canal || !voz.servidor) return;
  escolherFonte();
}

async function iniciarPartilha(fonte) {
  if (voz.ecra || !voz.canal || !voz.servidor) return;
  partilhaFalhou = null;
  partilhaAviso = null;

  // O canal fica aberto desde já, mas o Rust só manda por ele quando estivermos mesmo
  // a olhar (ver_meu_ecra). Criar o <video> agora e deixá-lo a apanhar pedaços às
  // escuras era o bug do ecrã preto: a fila aparava os antigos e o cabeçalho ia fora.
  const canal = new window.__TAURI__.core.Channel();
  canal.onmessage = pedaco => {
    if (voz.vejoMeuEcra) voz.vejoMeuEcra.empurrar(
      pedaco instanceof ArrayBuffer ? new Uint8Array(pedaco) : new Uint8Array(pedaco));
  };

  const q = qualidadeEfetiva();
  let medido = null;
  try {
    medido = await invoke('comecar_a_partilhar', {
      servidor: voz.servidor,
      canalVoz: voz.canal,
      fonte,
      altura: q.altura,
      fps: q.fps,
      debito: q.debito,
      comSom: q.som,
      saida: canal,
    });
  } catch (e) {
    console.warn('não consegui começar a partilhar:', e);
    return;
  }
  // O tamanho REAL com que a captura ficou. Com "Nativa" é a única forma de dizer a quem
  // assiste que resolução está a receber — só o Rust o soube calcular.
  voz.ecraTamanho = medido && Number.isFinite(medido.altura) ? medido : null;
  voz.qualidadeEmUso = { altura: q.altura, fps: q.fps, debito: q.debito, som: q.som };
  voz.ecra = { fechar() {} };
  // O tamanho REAL com que a captura ficou. Com "Nativa" é a única forma de dizer a quem
  // assiste que resolução está a receber, porque só o Rust o soube calcular.
  try {
    const r = await invoke('capacidades', { linha: '' }).catch(() => null);
    void r;
  } catch (e) { /* não faz mal */ }
  anunciarEstado();
  desenharVoz();
  desenharRodape();
}

/* --- receber o ecrã dos outros -------------------------------------------- */

/* Um canal só, e é o cabeçalho de cada pedaço que diz de quem ele é. O Rust põe à frente
   o tamanho da chave e a chave; o resto são os bytes do vídeo. */
const fluxosRecebidos = new Map();

(function ligarEntradaDeEcra() {
  if (!window.__TAURI__) return;
  const canal = new window.__TAURI__.core.Channel();
  canal.onmessage = pedaco => {
    const bytes = pedaco instanceof ArrayBuffer ? new Uint8Array(pedaco) : new Uint8Array(pedaco);
    if (!bytes.length) return;
    const n = bytes[0];
    if (bytes.length < 1 + n) return;
    const chave = new TextDecoder().decode(bytes.subarray(1, 1 + n));
    const corpo = bytes.subarray(1 + n);
    let fluxo = fluxosRecebidos.get(chave);
    if (!fluxo) {
      // Com som: é o ecrã de outra pessoa, e o que ela partilhou inclui o que se ouvia.
      fluxo = fluxoDePedacos(true);
      fluxosRecebidos.set(chave, fluxo);
      // O painel só sabe que há imagem depois do primeiro pedaço.
      desenharVoz();
    }
    fluxo.empurrar(corpo);
  };
  invoke('receber_ecra', { canal }).catch(() => {});
})();

/** Diz ao Rust quem está mesmo a ver. Enquanto isto estiver vazio, nada sai da máquina. */
function actualizarEspectadores() {
  if (!voz.ecra) return;
  // Neste momento quem assiste é quem tem a transmissão aberta; a interface ainda não
  // distingue "aberto mas minimizado", e é aí que vive a próxima poupança de upload.
  const lista = [...voz.aSerVistoPor].filter(p => voz.presentes.get(p) === voz.canal);
  invoke('definir_espectadores', { chaves: lista }).catch(() => {});
}

/** O nome LOCAL manda sobre o que a pessoa escolheu chamar-se.
 *
 *  O painel diz que ninguém garante que uma chave é de quem julgas, e que a defesa é
 *  compará-la por outro caminho e marcá-la como verificada. Só que o nome verificado vivia
 *  no ecrã dos Amigos e mais lado nenhum: em cada mensagem, na lista de membros e na
 *  chamada, aparecia o nome que a OUTRA pessoa escreveu — que é exactamente o campo que um
 *  impostor controla. A defesa existia e não se via onde é precisa.
 */
function nomeDoPeer(peer) {
  if (peer === voz.eu) return 'tu';
  const amigo = (amigos || []).find(a => a.chave === peer);
  if (amigo && amigo.nome) return amigo.nome;
  // Varre TODOS os servidores, e não só o que está aberto. Numa conversa privada não há
  // servidor aberto de onde tirar o nome, e mesmo num servidor a pessoa pode ser conhecida
  // de outra sala.
  for (const s of (vista && vista.servidores) || []) {
    const m = s.membros.find(x => x.chave === peer);
    if (m) return m.nome;
  }
  const c = ((vista && vista.conversas) || []).find(x => x.com === peer);
  return c ? c.nome : `${peer.slice(0, 6)}…`;
}

/** Um painel da grelha da chamada.
 *
 *  Três estados possíveis, e são mesmo diferentes:
 *   - a transmitir: a foto sai da frente e fica o convite para assistir;
 *   - com vídeo a ser visto: o vídeo ocupa tudo;
 *   - sem vídeo: a foto, com anel verde quando a pessoa fala.
 */
/** O `<video>` da transmissão de ecrã, se houver.
 *
 *  O ecrã já não é um MediaStream: vem em pedaços de MP4 e vive num `<video>` próprio,
 *  criado uma vez e reaproveitado. Redesenhar o painel não pode criar um novo, senão
 *  perdia-se tudo o que já foi recebido a cada mudança de ecrã.
 */
function ecraDe(chave) {
  if (chave === voz.eu) return voz.vejoMeuEcra ? voz.vejoMeuEcra.el : null;
  const f = fluxosRecebidos.get(chave);
  return f ? f.el : null;
}

/** Alguém está mesmo nesta sala? É o que resta de "há ligação a esta pessoa" agora que
 *  não há PeerConnections para consultar. */
function estaNaSala(chave) {
  return chave === voz.eu || voz.presentes.get(chave) === voz.canal;
}

function fecharFluxoRecebido(chave) {
  const f = fluxosRecebidos.get(chave);
  if (!f) return;
  f.fechar();
  fluxosRecebidos.delete(chave);
}

function painelDeVoz(chave, opcoes = {}) {
  const t = elemento('div', 'tile');
  t.dataset.chave = chave;
  if (voz.falando.has(chave)) t.classList.add('a-falar');

  const transmite = voz.aPartilhar.has(chave) || (chave === voz.eu && !!voz.ecra);
  const aVer = opcoes.aVer;
  const temVideo = voz.comCamara.has(chave) || (chave === voz.eu && !!voz.camara);

  if (transmite && !aVer) {
    // Enquanto não se carrega em Assistir, não se descodifica nada: poupa CPU de quem
    // está na sala só para ouvir, que é a maioria das vezes.
    const bloco = elemento('div', 'tile__transmite');
    const marca = elemento('span', 'ident');
    pintar(marca, chave);
    bloco.append(marca);
    bloco.append(elemento('b', null, `${nomeDoPeer(chave)} está a transmitir`));
    const b = elemento('button', 'btn btn--primary',
      chave === voz.eu ? 'Ver o que estás a enviar' : 'Assistir');
    b.onclick = () => assistir(chave);
    bloco.append(b);
    if (chave === voz.eu) {
      bloco.append(elemento('span', 'tile__dica', 'é o teu ecrã, tal como sai daqui'));
    }
    t.append(bloco);
  } else if (transmite && aVer) {
    // O <video> do ecrã é reaproveitado, nunca recriado: ele já tem dentro tudo o que
    // chegou até agora, e criar outro aqui deitava isso fora a cada redesenho.
    const el = ecraDe(chave);
    if (el) t.append(el);
    else t.append(elemento('div', 'tile__sem-video', 'à espera da imagem…'));
  } else if (temVideo) {
    // A câmara ganha ao avatar mas perde para a transmissão: quem está a mostrar o ecrã
    // está a mostrá-lo por alguma razão, e a cara cabe no painel de quem não está.
    const el = chave === voz.eu ? meuEspelho() : camaraDe(chave).tela;
    if (el) t.append(el);
  } else {
    const sem = elemento('div', 'tile__sem-video');
    const av = elemento('span', 'ident');
    pintar(av, chave);
    sem.append(av);
    // Só se escreve alguma coisa quando ainda NÃO há ligação. "Só áudio" seria
    // ruído: a foto sozinha já diz que não há vídeo.
    if (!estaNaSala(chave)) sem.append(elemento('span', null, 'a ligar…'));
    t.append(sem);
  }

  const partido = vozPartida.get(chave);
  if (partido) {
    // Sem isto, quem deixa de ouvir uma pessoa suspeita DELA — e ela continua ali, a
    // aparecer presente e sem noção de nada.
    const aviso = elemento('span', 'tile__sem-audio', 'sem áudio');
    aviso.title = partido;
    t.append(aviso);
  } else {
    // E O CASO MAIS PROVÁVEL, que não é o descodificador falhar (#165).
    //
    // É o codificador DELA ter morrido, o microfone dela ter desaparecido, ou os datagramas
    // dela deixarem de chegar. Nesse caso ela continuava no painel, sem anel, exactamente
    // igual a quem está calado de propósito. E são dois problemas com respostas opostas:
    // a um espera-se, ao outro avisa-se a pessoa.
    //
    // Sessenta segundos e não trinta: com um portão no emissor, um minuto de silêncio real
    // é possível, e a marca a aparecer em quem só ouve seria pior do que não existir.
    const q = ultimoEstadoDaVoz.find(x => x.peer === chave);
    if (q && q.recebidos > 0 && typeof q.haQuantoRec === 'number' && q.haQuantoRec > 60000) {
      const min = Math.round(q.haQuantoRec / 60000);
      const mudo = elemento('span', 'tile__sem-audio',
        min <= 1 ? 'sem som há 1 min' : `sem som há ${min} min`);
      mudo.title = 'Chegou som desta pessoa antes, e agora não chega. Ela pode estar a falar '
        + 'sem saber que não sai nada.';
      t.append(mudo);
    }
  }
  t.append(elemento('span', 'tile__nome', nomeDoPeer(chave)));
  t.append(accoesDoPainel(chave, { transmite, aVer, temVideo }));
  return t;
}

/** Quantos, ao todo, estão a ver a transmissão de `quem` — incluindo eu. */
function espectadoresDe(quem) {
  if (quem === voz.eu) return voz.aSerVistoPor.size + (voz.aVer === voz.eu ? 1 : 0);
  const i = voz.infoDaTransmissao.get(quem);
  return i ? i.espectadores : 0;
}

function qualidadeDe(quem) {
  if (quem === voz.eu) return rotuloDaQualidade();
  const i = voz.infoDaTransmissao.get(quem);
  return i && i.qualidade ? i.qualidade : null;
}

/** Se as fotinhas por baixo do palco estão escondidas. */
let gentePorBaixoOculta = false;

/** Se a janela do Bruma tem o foco — ver a pausa da pré-visualização no palco. */
window.addEventListener('focus', () => { janelaComFoco = true; if (voz.aVer) desenharVoz(); });
window.addEventListener('blur', () => { janelaComFoco = false; if (voz.aVer) desenharVoz(); });

const ICO = {
  mic: '<path d="M10 3.2a2.1 2.1 0 0 1 2.1 2.1v4.4a2.1 2.1 0 0 1-4.2 0V5.3A2.1 2.1 0 0 1 10 3.2Z"/><path d="M5.4 9.3a4.6 4.6 0 0 0 9.2 0M10 13.9v2.9"/>',
  camara: '<rect x="2.6" y="5.4" width="10" height="9.2" rx="2"/><path d="M12.6 9.2l4.8-2.6v6.8l-4.8-2.6Z"/>',
  gente: '<circle cx="7.6" cy="7.4" r="2.6"/><path d="M3 16.2c0-2.5 2.1-4 4.6-4s4.6 1.5 4.6 4"/><path d="M13.4 5.2a2.4 2.4 0 0 1 0 4.5M14.4 12.6c1.7.4 2.9 1.6 2.9 3.6"/>',
  parar: '<rect x="4.6" y="4.6" width="10.8" height="10.8" rx="2.2"/>',
  desligar: '<path d="M4.2 8.4c3.4-2.2 8.2-2.2 11.6 0l.9 2.3-3 .5-.6-1.9c-1.9-.8-4.3-.8-6.2 0l-.6 1.9-3-.5Z"/>',
  convidar: '<circle cx="8" cy="7.2" r="2.7"/><path d="M3.3 16c0-2.6 2.2-4.2 4.7-4.2 1 0 1.9.2 2.7.7"/><path d="M14.6 11.4v4.8M12.2 13.8h4.8"/>',
  olho: '<circle cx="6" cy="5.6" r="2.1"/><path d="M2 13c0-2.1 1.8-3.4 4-3.4s4 1.3 4 3.4"/><path d="M11 4.4a2 2 0 0 1 0 3.8M12 10.2c1.4.3 2.4 1.3 2.4 2.9"/>',
};

/** Um botão redondo da barra flutuante. */
function botaoDoPalco(desenho, titulo, aoClicar, opcoes) {
  opcoes = opcoes || {};
  const b = elemento('button', 'palco__bt' + (opcoes.classe ? ' ' + opcoes.classe : ''));
  b.title = titulo;
  b.setAttribute('aria-label', titulo);
  b.innerHTML = '<svg viewBox="0 0 20 20" width="17" height="17">' + desenho + '</svg>';
  if (opcoes.cortado) b.classList.add('is-cortado');
  b.onclick = aoClicar;
  return b;
}

/** O palco: a transmissão a ocupar tudo, com as camadas por cima e as fotinhas por baixo.
 *
 *  # Porque é que tudo flutua
 *
 *  Uma transmissão de 3440x1440 numa janela já é pequena; roubar-lhe barras fixas em cima
 *  e em baixo custava mais do que aquilo que essas barras dizem. Aparecem com o rato por
 *  cima e saem sozinhas, como em qualquer leitor a sério — e o `:focus-within` garante que
 *  quem anda pelo teclado também lhes chega.
 */
function palcoDeTransmissao(quem, canal, outros) {
  const souEu = quem === voz.eu;
  const palco = elemento('div', 'palco');
  const vidro = elemento('div', 'palco__vidro');

  const el = ecraDe(quem);
  const fluxo = quem === voz.eu ? voz.vejoMeuEcra : fluxosRecebidos.get(quem);
  const recusa = fluxo && fluxo.porqueNaoDa && fluxo.porqueNaoDa();
  if (recusa) {
    // Um rectângulo preto para sempre é indistinguível de "a rede está má". Dizer a razão
    // é a diferença entre a pessoa esperar em vão e saber que não vale a pena.
    const caixa = elemento('div', 'palco__pausa');
    caixa.append(elemento('b', null, 'Não consigo mostrar esta transmissão'));
    caixa.append(elemento('span', null, recusa));
    vidro.append(caixa);
  } else if (el) {
    vidro.append(el);
  } else {
    vidro.append(elemento('div', 'tile__sem-video', 'à espera da imagem…'));
  }

  // --- cima à esquerda: onde estou e de quem é isto
  const onde = elemento('div', 'palco__onde');
  onde.append(elemento('b', null, canal ? canal.nome : 'Sala'));
  onde.append(elemento('i', null, souEu ? 'a tua transmissão' : nomeDoPeer(quem)));
  const camadaEsq = elemento('div', 'palco__camada palco__cima-esq');
  // A saída. Não estava no pedido, mas sem ela fica-se preso no palco: as fotinhas de
  // baixo levam a OUTRAS transmissões, nenhuma leva de volta à sala.
  camadaEsq.append(botaoDoPalco(
    '<path d="M12 4.5 6.5 10l5.5 5.5"/>', 'Voltar à sala', pararDeAssistir));
  camadaEsq.append(onde);
  vidro.append(camadaEsq);

  // --- cima à direita: qualidade, quantos veem, e o AO VIVO
  const camadaDir = elemento('div', 'palco__camada palco__cima-dir');
  const qual = qualidadeDe(quem);
  if (qual) {
    const selo = elemento('span', 'palco__selo', qual);
    // A explicação vive AQUI e não numa nota à parte: é neste número que a pessoa repara
    // quando ele não é o que ela escolheu, e é aqui que a pergunta nasce.
    const porque = souEu ? porqueEstaResolucao() : null;
    selo.title = porque || 'a resolução e o ritmo desta transmissão';
    if (porque && voz.qualidadeEmUso && voz.ecraTamanho
        && voz.qualidadeEmUso.altura && voz.ecraTamanho.altura < voz.qualidadeEmUso.altura) {
      selo.classList.add('palco__selo--nota');
    }
    camadaDir.append(selo);
  }
  const olhos = elemento('span', 'palco__selo');
  olhos.innerHTML =
    '<svg viewBox="0 0 16 16" width="12" height="12" fill="none" stroke="currentColor"'
    + ' stroke-width="1.4">' + ICO.olho + '</svg>';
  olhos.append(elemento('span', null, String(espectadoresDe(quem))));
  camadaDir.append(olhos);
  const vivo = elemento('span', 'palco__selo palco__selo--vivo');
  vivo.innerHTML = '<i class="ponto"></i>';
  vivo.append(document.createTextNode('AO VIVO'));
  camadaDir.append(vivo);
  vidro.append(camadaDir);

  // --- baixo à esquerda: convidar alguém a ver
  const camadaConv = elemento('div', 'palco__camada palco__camada--baixo palco__baixo-esq');
  camadaConv.append(botaoDoPalco(ICO.convidar, 'Convidar para assistir', () => {
    // O convite é o do servidor: quem entra por ele chega à sala e vê o que está a dar.
    $('#btn-convite').click();
  }));
  vidro.append(camadaConv);

  // --- baixo ao meio: os controlos
  const barra = elemento('div', 'palco__camada palco__camada--baixo palco__meio');
  const faixaMic = voz.micro ? voz.micro.getAudioTracks()[0] : null;
  barra.append(botaoDoPalco(
    ICO.mic,
    !faixaMic ? 'Sem microfone' : (faixaMic.enabled ? 'Silenciar microfone' : 'Ligar microfone'),
    () => $('#btn-mic').click(),
    { cortado: !!faixaMic && !faixaMic.enabled },
  ));
  barra.append(botaoDoPalco(
    ICO.camara,
    voz.camara ? 'Desligar a câmara' : 'Ligar a câmara',
    () => $('#btn-camara').click(),
    { cortado: !voz.camara },
  ));
  // O de esconder as fotinhas fica NO MEIO, como pedido: é o que separa os controlos de
  // quem fala dos que fecham a transmissão e a chamada.
  barra.append(botaoDoPalco(
    ICO.gente,
    gentePorBaixoOculta ? 'Mostrar quem está na chamada' : 'Ocultar quem está na chamada',
    () => { gentePorBaixoOculta = !gentePorBaixoOculta; desenharVoz(); },
    { cortado: gentePorBaixoOculta },
  ));
  if (souEu) {
    barra.append(botaoDoPalco(ICO.parar, 'Parar a transmissão',
      () => alternarEcra(), { classe: 'palco__bt--parar' }));
  }
  barra.append(botaoDoPalco(
    ICO.desligar,
    souEu ? 'Sair da chamada (fecha a transmissão)' : 'Sair da chamada',
    () => sairDeVoz(),
    { classe: 'palco__bt--sair' },
  ));
  vidro.append(barra);

  // --- a pausa por falta de foco, só para quem transmite
  //
  // A transmissão continua a sair; o que descansa é o DESCODIFICADOR local. Quem partilha
  // está a codificar o ecrã inteiro e, ao mesmo tempo, a descodificá-lo outra vez só para
  // se ver — e quando muda de janela nem sequer está a olhar. Pausar aqui devolve esse
  // trabalho ao jogo, ou ao que quer que ele tenha ido fazer.
  // A MENSAGEM não depende de haver imagem: ela é verdadeira mal a transmissão comece, e
  // prendê-la ao <video> deixava quem mudasse de janela nos primeiros segundos sem
  // explicação nenhuma para o ecrã escuro.
  if (souEu && !janelaComFoco) {
    const pausa = elemento('div', 'palco__pausa');
    pausa.append(elemento('b', null, 'A tua transmissão continua ligada!'));
    pausa.append(elemento('span', null,
      'Pausámos esta pré-visualização para poupar os teus recursos.'));
    vidro.append(pausa);
  }
  if (souEu && el) {
    if (!janelaComFoco) {
      el.__pausaPedida = true;
      try { el.pause(); } catch (e) { /* já parado */ }
    } else if (el.__pausaPedida) {
      el.__pausaPedida = false;
      // Ao voltar, salta-se logo para a ponta: o que passou enquanto estava pausado não
      // interessa a ninguém, e reproduzi-lo seria voltar ao atraso que se acabou de curar.
      try {
        if (el.buffered.length) el.currentTime = el.buffered.end(el.buffered.length - 1) - 0.4;
      } catch (e) { /* o perseguidor do vivo trata */ }
      el.play().catch(() => {});
    }
  }

  palco.append(vidro);

  // --- as fotinhas por baixo
  const fila = elemento('div', 'palco__gente');
  fila.hidden = gentePorBaixoOculta;
  fila.append(fotinha(voz.eu, quem));
  for (const p of outros) fila.append(fotinha(p, quem));
  palco.append(fila);
  return palco;
}

/** Uma fotinha da fila de baixo. `noPalco` é quem está em grande, para não se repetir. */
function fotinha(chave, noPalco) {
  const m = elemento('div', 'mini');
  m.dataset.chave = chave;
  if (voz.falando.has(chave)) m.classList.add('is-speaking');

  const transmite = voz.aPartilhar.has(chave) || (chave === voz.eu && !!voz.ecra);
  const temCamara = voz.comCamara.has(chave) || (chave === voz.eu && !!voz.camara);

  if (temCamara) {
    const cam = chave === voz.eu ? meuEspelho() : camaraDe(chave).tela;
    if (cam) m.append(cam);
  } else {
    const av = elemento('span', 'ident');
    pintar(av, chave);
    m.append(av);
  }

  if (transmite) {
    m.append(elemento('span', 'mini__vivo', 'AO VIVO'));
    // Quem transmite mas NÃO está no palco: diz-se, e o botão de assistir só aparece com o
    // rato por cima — para a fila não virar uma parede de botões.
    if (chave !== noPalco) {
      const tampa = elemento('div', 'mini__tampa');
      tampa.append(elemento('span', null,
        chave === voz.eu ? 'estás a transmitir' : nomeDoPeer(chave) + ' está a transmitir'));
      const b = elemento('button', 'btn btn--primary',
        chave === voz.eu ? 'Ver a tua' : 'Assistir');
      b.onclick = ev => { ev.stopPropagation(); assistir(chave); };
      tampa.append(b);
      m.append(tampa);
    }
  }
  const partida = vozPartida.get(chave);
  if (partida) {
    const aviso = elemento('span', 'mini__vivo', 'SEM ÁUDIO');
    aviso.style.background = 'var(--amber)';
    aviso.style.color = '#16181c';
    aviso.title = partida;
    m.append(aviso);
  }
  m.append(elemento('span', 'mini__nome', nomeDoPeer(chave)));
  return m;
}

/** Dimensiona a grelha da chamada.
 *
 *  O CSS sozinho não chega: com `auto-fit` uma pessoa sozinha ficava com um painel do
 *  tamanho do ecrã e a foto perdida no meio. Aqui calcula-se quantas colunas fazem sentido
 *  e qual o lado máximo que ainda deixa todas as linhas caberem na altura disponível —
 *  portanto os painéis encolhem sozinhos à medida que as pessoas entram, e nunca há scroll.
 */
function ajustarGrelha(n) {
  const g = $('#voz-grelha');
  if (!g || n < 1) return;
  const ESPACO = 12;
  const RACIO = 16 / 10;
  const MAXIMO = 460;   // uma pessoa sozinha não precisa de um painel gigante

  const colunas = Math.ceil(Math.sqrt(n));
  const linhas = Math.ceil(n / colunas);
  const alturaUtil = (g.clientHeight - (linhas - 1) * ESPACO) / linhas;
  const larguraUtil = (g.clientWidth - (colunas - 1) * ESPACO) / colunas;
  const lado = Math.min(alturaUtil * RACIO, larguraUtil, MAXIMO);

  g.style.setProperty('--colunas', colunas);
  g.style.setProperty('--lado', `${Math.max(140, Math.floor(lado))}px`);
}

/** Os botões que aparecem ao passar o rato num painel — e só os que fazem sentido para
 *  aquela pessoa naquele momento. */
function accoesDoPainel(chave, { transmite, aVer, temVideo }) {
  const barra = elemento('div', 'tile__acoes');
  const botao = (rotulo, titulo, accao, ligado) => {
    const b = elemento('button', ligado ? 'tile__bt is-on' : 'tile__bt', rotulo);
    b.title = titulo;
    b.onclick = ev => { ev.stopPropagation(); accao(); };
    return b;
  };

  if (transmite && chave !== voz.eu) {
    barra.append(aVer
      ? botao('▣', 'Voltar à sala', pararDeAssistir)
      : botao('▸', 'Assistir à transmissão', () => assistir(chave)));
  }

  if (aVer && temVideo) {
    barra.append(botao('⛶', 'Ecrã inteiro', () => {
      const v = document.querySelector(`.tile[data-chave="${chave}"] video`);
      if (v) (document.fullscreenElement ? document.exitFullscreen() : v.requestFullscreen());
    }));
  }

  if (chave !== voz.eu) {
    const mudo = voz.silenciados.has(chave);
    barra.append(botao(mudo ? '🔇' : '🔊', mudo ? 'Voltar a ouvir' : 'Silenciar esta pessoa', () => {
      if (mudo) voz.silenciados.delete(chave); else voz.silenciados.add(chave);
      // Baixa-se o ganho dessa pessoa e mais nada: silenciar alguém é uma decisão de quem
      // ouve, e não deve mexer no que os outros recebem.
      ajustarVolume(chave);
      desenharVoz();
    }, mudo));
  }

  return barra;
}

function desenharVoz() {
  const s = servidor();
  const canal = s && s.canais.find(c => c.id === canalAtual);
  // A vista de voz vive no modo servidor. Sem esta condição ela lia o `canalAtual` velho —
  // o da última sala onde se esteve — e abria-se por cima da conversa privada.
  const eDeVoz = modo === 'servidor' && canal && canal.tipo === 'voz';
  $('#vista-voz').hidden = !eDeVoz;
  desenharNaChamada();
  if (!eDeVoz) return;

  const ligado = voz.canal === canal.id;
  const outros = [...voz.presentes.entries()].filter(([, c]) => c === canal.id).map(([p]) => p);

  const grelha = $('#voz-grelha');
  grelha.textContent = '';
  grelha.classList.toggle('esta-a-ver', !!voz.aVer);

  if (!ligado) {
    const v = elemento('div', 'vazio');
    v.append(elemento('h3', null, canal.nome));
    v.append(elemento('p', null, 'Entra para falar e partilhar o ecrã com quem estiver aqui.'));
    const b = elemento('button', 'btn btn--primary', 'Entrar na sala');
    b.onclick = () => entrarEmVoz(s.id, canal.id);
    v.append(b);
    grelha.append(v);
    $('#voz-nota').textContent = '';
    return;
  }

  // A ver a transmissão de alguém: a imagem ocupa tudo e o resto flutua por cima.
  if (voz.aVer) {
    grelha.append(palcoDeTransmissao(voz.aVer, canal, outros));
    $('#voz-nota').textContent = '';
    return;
  }

  ajustarGrelha(outros.length + 1);
  grelha.append(painelDeVoz(voz.eu));
  for (const p of outros) grelha.append(painelDeVoz(p));

  // Já não há nada para configurar: a voz e o ecrã vão os dois pelo iroh, que trata do
  // NAT sozinho. Esta nota chegou a explicar como pôr um TURN a funcionar; hoje seria
  // explicar um problema que deixou de existir.
  $('#voz-nota').textContent = '';
}

/** O chat da sala de voz, na coluna da direita.
 *
 *  É um canal como os outros — as mensagens ficam no mesmo registo assinado, com o id da
 *  sala de voz como canal, portanto ficam mesmo separadas das dos canais de texto e o
 *  histórico sobrevive a sair e voltar.
 *
 *  O que aqui se faz é escondê-lo de quem não está na sala. Convém dizer com todas as
 *  letras o que isso é e o que não é: é uma regra desta app, não da criptografia. A
 *  mensagem viaja cifrada com a chave do servidor, a mesma de tudo o resto, por isso
 *  chega ao computador de todos os membros e um cliente modificado conseguia lê-la sem
 *  nunca entrar na sala. Para ser garantia a sério a sala precisava de chave própria —
 *  está dito no painel do "?" ao lado do título, para ninguém confiar a mais.
 */
async function desenharChatDaSala() {
  const alvo = $('#sala-chat');
  if (!alvo) return;
  if (!voz.canal || !voz.servidor) { alvo.hidden = true; return; }

  const s = vista.servidores.find(x => x.id === voz.servidor);
  const canal = s && s.canais.find(c => c.id === voz.canal);
  if (!canal) { alvo.hidden = true; return; }

  alvo.hidden = false;
  $('#sala-chat-nome').textContent = `Chat · ${canal.nome}`;
  $('#sala-entrada').placeholder = `Mensagem para ${canal.nome}`;

  const fluxo = $('#sala-fluxo');
  const colado = fluxo.scrollHeight - fluxo.scrollTop - fluxo.clientHeight < 40;
  const msgs = await invoke('mensagens', { servidor: s.id, canal: canal.id }).catch(() => []);

  fluxo.textContent = '';
  if (!msgs.length) {
    fluxo.append(elemento('div', 'salachat__vazio',
      'Só quem está nesta sala vê este chat.'));
    return;
  }
  for (const m of msgs) {
    const linha = elemento('div', 'salachat__msg');
    linha.append(elemento('span', 'salachat__quem', m.autor_nome));
    linha.append(elemento('span', 'salachat__txt', m.texto));
    fluxo.append(linha);
  }
  // Só se salta para o fim se já lá estavas: senão roubava-te a leitura a meio.
  if (colado) fluxo.scrollTop = fluxo.scrollHeight;
}

$('#sala-entrada').addEventListener('keydown', async ev => {
  if (ev.key !== 'Enter' || !ev.target.value.trim()) return;
  if (!voz.canal || !voz.servidor) return;
  const texto = ev.target.value;
  ev.target.value = '';
  try {
    await invoke('enviar', { servidor: voz.servidor, canal: voz.canal, texto });
    await desenharChatDaSala();
  } catch (e) { console.error(e); }
});

/** A lista lateral de quem está na chamada, com o anel verde de quem fala. */
function desenharNaChamada() {
  desenharChatDaSala();
  // Na chamada, a coluna da direita é só da chamada: quem lá está e o chat da sala. A
  // lista geral de membros volta assim que saíres — ali dentro não acrescentava nada e
  // roubava a altura ao chat.
  const membros = $('#bloco-membros');
  if (membros) membros.hidden = !!voz.canal;

  const alvo = $('#na-chamada');
  if (!alvo) return;
  if (!voz.canal) { alvo.hidden = true; alvo.textContent = ''; return; }

  const s = vista.servidores.find(x => x.id === voz.servidor);
  const canal = s && s.canais.find(c => c.id === voz.canal);
  const gente = [voz.eu, ...[...voz.presentes.entries()]
    .filter(([, c]) => c === voz.canal).map(([p]) => p)];

  alvo.hidden = false;
  alvo.textContent = '';
  alvo.append(elemento('div', 'members__label',
    `Na chamada · ${canal ? canal.nome : ''}`));

  for (const p of gente) {
    const linha = elemento('div', 'member member--chamada');
    linha.dataset.chave = p;
    if (voz.falando.has(p)) linha.classList.add('a-falar');
    const av = elemento('span', 'ident');
    pintar(av, p);
    const bloco = elemento('span');
    bloco.append(elemento('b', null, nomeDoPeer(p)));
    const transmite = voz.aPartilhar.has(p) || (p === voz.eu && !!voz.ecra);
    bloco.append(elemento('i', null, transmite ? 'a transmitir' : 'na chamada'));
    linha.append(av, bloco);
    if (transmite) {
      const b = elemento('button', 'chan__x chan__x--ver', '▸');
      b.title = p === voz.eu ? 'Ver o que estás a enviar' : 'Assistir';
      b.onclick = ev => { ev.stopPropagation(); assistir(p); };
      linha.append(b);
    }
    alvo.append(linha);
  }
}

/* A captura pode morrer DEPOIS de o comando já ter dito que sim — uma definição que este
   Windows não tem, o ecrã a desaparecer, o codificador a recusar. Sem isto, a interface
   ficava a dizer "estás a partilhar" para sempre, sem imagem, sem explicação e sem forma
   de a pessoa perceber que o problema não era a ligação dela. */
/* O que não impede a partilha mas a pessoa tem de saber — hoje, um Windows que não deixa
   separar o som da app do resto e portanto devolve a voz da chamada. Fica no botão, que é
   onde ela vai olhar, e não numa consola que não existe. */
/** Erro de escrita no disco — uma faixa PERSISTENTE, porque um disco cheio não se resolve
 *  sozinho. Fica até a app fechar. É a diferença entre uma falha silenciosa total (o que
 *  havia) e o utilizador saber que o que escrever a partir de agora pode perder-se. */
/** Um aviso efémero na sala ou conversa a que diz respeito.
 *
 *  Não é uma mensagem: não vai para o log, não é assinado, e desaparece ao recarregar. É o
 *  mesmo tipo de coisa que a presença já é — um facto sobre AGORA, não história. Escrever
 *  estes avisos no log seria pôr no histórico de toda a gente uma coisa que só a esta
 *  máquina diz respeito.
 */
function avisoNaConversa(id, texto) {
  // O REDESENHO CHEGA PRIMEIRO, E É POR ISSO QUE ISTO ESPERA.
  //
  // O `aplicar` do lado do Rust emite `servidor-mudou` e logo a seguir o aviso. O primeiro
  // dispara um `desenharTudo`, que limpa o `#stream` e o reconstrói — e limpava o aviso
  // milissegundos depois de ele ser escrito. Ninguém o via, e eu tinha-o dado por feito.
  //
  // Guarda-se numa fila por sala e pinta-se DEPOIS do redesenho, com um `setTimeout(0)` que
  // o põe atrás do trabalho já agendado. A fila é o que faz o aviso sobreviver a um
  // redesenho que ainda não aconteceu, em vez de depender de quem chega primeiro.
  const fila = avisosPendentes.get(id) || [];
  fila.push(texto);
  avisosPendentes.set(id, fila);
  setTimeout(pintarAvisos, 0);
}

/** Os avisos por sala/conversa, à espera de caber no ecrã. */
const avisosPendentes = new Map();

function pintarAvisos() {
  const zona = $('#stream');
  if (!zona) return;
  // Só se mostram os da sala/conversa ABERTA — um aviso sobre outra sala no meio desta
  // conversa seria ruído no sítio errado. Os outros ficam na fila até lá se ir.
  const aberto = modo === 'privado' ? conversaAtual : servidorAtual;
  const fila = avisosPendentes.get(aberto);
  if (!fila || !fila.length) return;
  for (const texto of fila) {
    // Não se repete o mesmo aviso: um redesenho a meio podia trazer a fila outra vez.
    if ([...zona.querySelectorAll('.aviso-sistema')].some(n => n.textContent === texto)) continue;
    zona.append(elemento('div', 'aviso-sistema', texto));
  }
  avisosPendentes.set(aberto, []);
  zona.scrollTop = zona.scrollHeight;
}

/* A RECUSA DITA (#131).
   Até aqui, quem era recusado pela política do outro lado não sabia: a conversa aparecia,
   as mensagens ficavam no log local, o envio saía sem erro, e a pessoa escrevia durante
   dias para uma sala que só existia na máquina dela. Agora diz-se, no sítio onde ela está
   a escrever. */
listen('conversa-recusada', ev => {
  const id = String(ev.payload || '');
  const c = (vista.conversas || []).find(x => x.id === id);
  const quem = c ? c.nome : 'Esta pessoa';
  avisoNaConversa(id, `${quem} só aceita conversas de quem já a conhece — o que escreveres `
    + 'aqui não lhe chega.');
});

/* ALGUÉM ENTROU NA SALA (#196).
   Passar a ser membro dá o direito de me pôr som nas colunas e de receber o meu ecrã. Era
   completamente mudo. Vai agrupado de propósito: numa sala que sincroniza histórico grande,
   muitos autores aparecem de uma vez. */
listen('membros-novos', ev => {
  const [servidor, chaves] = ev.payload || [];
  if (!servidor || !chaves || !chaves.length) return;
  const s = (vista.servidores || []).find(x => x.id === servidor);
  const nomeDe = k => {
    const m = s && (s.membros || []).find(x => x.chave === k);
    return m ? m.nome : chaveCurta(k);
  };
  const texto = chaves.length === 1
    ? `${nomeDe(chaves[0])} apareceu nesta sala.`
    : `${chaves.length} pessoas apareceram nesta sala.`;
  avisoNaConversa(servidor, texto);
});

listen('erro-dados', ev => {
  const texto = String(ev.payload || 'Não consigo escrever na pasta de dados.');
  let faixa = document.getElementById('faixa-disco');
  if (!faixa) {
    faixa = document.createElement('div');
    faixa.id = 'faixa-disco';
    faixa.className = 'faixa-disco';
    const app = document.querySelector('.app');
    if (app) app.prepend(faixa); else document.body.prepend(faixa);
  }
  faixa.textContent = '⚠ ' + texto;
});

listen('partilha-aviso', ev => {
  partilhaAviso = String(ev.payload || '');
  console.warn('aviso da partilha:', partilhaAviso);
  desenharRodape();
});

/** Um aviso sobre a partilha em curso, que não a impede. */
let partilhaAviso = null;

listen('partilha-falhou', ev => {
  const razao = String(ev.payload || 'a captura parou');
  console.warn('a partilha falhou:', razao);
  if (voz.vejoMeuEcra) { voz.vejoMeuEcra.fechar(); voz.vejoMeuEcra = null; }
  if (voz.aVer === voz.eu) voz.aVer = null;
  voz.ecra = null; voz.ecraTamanho = null; voz.qualidadeEmUso = null;
  voz.aSerVistoPor.clear();   // a partilha caiu: os pedidos de assistir caem com ela
  partilhaFalhou = razao;
  invoke('capacidades', { linha: `partilha-falhou chegou a UI: ${razao}` }).catch(() => {});
  invoke('parar_de_partilhar').catch(() => {});
  anunciarEstado();
  desenharVoz();
  desenharRodape();
});

/** A última razão por que a partilha morreu, para o botão a poder mostrar. */
let partilhaFalhou = null;

listen('presenca', ev => {
  const { peer, canal } = ev.payload;
  if (canal) voz.presentes.set(peer, canal); else voz.presentes.delete(peer);
  // Já não há ligação nenhuma a abrir: quem chega passa a existir para nós assim que o
  // primeiro pedaço de voz dele aparecer, e o Rust já tem a ligação do iroh de pé.
  if (voz.canal && canal === voz.canal) anunciarEstado();
  if (!canal || canal !== voz.canal) {
    if (voz.aSerVistoPor.delete(peer)) actualizarEspectadores();
    fecharFluxoRecebido(peer);
    // E a câmara também. Quem cai da rede não chega a anunciar que a desligou, e sem isto
    // o descodificador dele ficava aberto para sempre a segurar memória de vídeo — numa
    // sala onde as pessoas entram e saem, isso só cresce.
    fecharCamaraRecebida(peer);
    voz.comCamara.delete(peer);
    voz.entendeCamara.delete(peer);
    voz.entendeSom.delete(peer);
    voz.jaFalou.delete(peer);
  }
  if (!canal || canal !== voz.canal) calarPeer(peer);
  desenharVoz();
  desenharCanais();
  desenharRodape();
});

listen('sinal', ev => {
  const { de, canal, dados } = ev.payload;
  if (canal !== voz.canal) return;
  try { receberSinal(de, JSON.parse(dados)); } catch (e) { console.error(e); }
});

/* ==========================================================================
   Rodapé: o que tens aberto, a ligação de voz, e os botões ao lado do nome.
   ========================================================================== */

let jogoAberto = null;

/** Quantas vezes já perguntámos ao Windows o que está aberto.
 *
 *  Existe para o `--medir-ui` poder provar que o interruptor "não olhar para as minhas
 *  janelas" cala mesmo a pergunta. Sem isto, a única forma de verificar era ler o código e
 *  acreditar -- e um interruptor de privacidade é precisamente onde acreditar não chega. */
let perguntasSobreJanelas = 0;

/* --- o que tens aberto ----------------------------------------------------- */

async function verJogo() {
  // Quem desliga isto está a pedir que não se olhe para as janelas dele. Esconder o cartão
  // e continuar a perguntar ao Windows o que está aberto seria dar-lhe a aparência da
  // privacidade sem a privacidade -- o pedido é sobre o que se pergunta, não sobre o que
  // se mostra. Por isso a saída é ANTES do invoke.
  if (deteccaoDeJogoDesligada()) {
    jogoAberto = null;
    $('#jogo').hidden = true;
    return;
  }
  try {
    perguntasSobreJanelas += 1;
    const j = await invoke('jogo_em_execucao');
    jogoAberto = j;
    const linha = $('#jogo');
    if (!j) { linha.hidden = true; return; }
    linha.hidden = false;
    $('#jogo-nome').textContent = j.titulo;
    pintar($('#jogo-marca'), j.processo);
    const aTransmitir = !!voz.ecra;
    $('#jogo-estado').textContent = aTransmitir ? 'A transmitir' : 'Não estás a transmitir';
    $('#btn-jogo').classList.toggle('is-on', aTransmitir);
    $('#btn-jogo').title = aTransmitir
      ? 'Parar de transmitir'
      : `Transmitir — escolhe "${j.titulo}" na janela que aparece`;
  } catch (e) {
    $('#jogo').hidden = true;
  }
}

$('#btn-jogo').onclick = async () => {
  // Não dá para começar a partilhar sem estar numa sala: não haveria a quem enviar.
  if (!voz.canal) {
    const s = servidor();
    const sala = s && s.canais.find(c => c.tipo === 'voz');
    if (!sala) return;
    canalAtual = sala.id;
    desenharCanais();
    desenharTopo();
    await desenharMensagens();
    await entrarEmVoz(s.id, sala.id);
  }
  if (voz.ecra) { alternarEcra(); return; }   // já a transmitir: o botão pára
  // O monitor-com-seta promete transmitir O JOGO, não abrir um menu: procura-se a
  // janela dele pelo título e só se cai no seletor se ela tiver desaparecido.
  const fontes = await invoke('fontes_de_partilha').catch(() => []);
  const doJogo = jogoAberto && fontes.find(f => f.tipo === 'janela' && f.titulo === jogoAberto.titulo);
  if (doJogo) iniciarPartilha(doJogo.id);
  else escolherFonte();
};

setInterval(verJogo, 5000);

/* --- ligação de voz --------------------------------------------------------- */

/** A qualidade da ligação, medida pelo próprio transporte.
 *
 *  Isto vinha das estatísticas do WebRTC. Agora vem do iroh, e é melhor informação: além
 *  do tempo de ida e volta, ele sabe dizer se a ligação é **direta** ou se está a passar
 *  por um relay — que é a diferença entre o router ter sido furado ou não, e a coisa mais
 *  útil que se pode mostrar a quem está a queixar-se de que "está lento".
 */
/* O último estado da voz, guardado para quem mais o quiser (o painel de rede, as marcas
   por pessoa). É preenchido pelo `qualidadeDaLigacao`, que já corre de segundo a segundo. */
let ultimoEstadoDaVoz = [];

/** O que o rodapé diz sobre a chamada — ancorado no TRÁFEGO, não no RTT.
 *
 *  # A promessa que isto deixa de fazer (#32, #171)
 *
 *  Isto decidia a frase inteira a partir de duas coisas: se a ligação era por relay, e o
 *  RTT. Nenhuma delas sabe se saiu ou entrou um pacote de som. O rodapé escrevia «Voz
 *  conectada · 180 ms» com o codificador morto, com o microfone desaparecido, ou com o
 *  `send_datagram` a falhar sempre — a promessa que o código não cumpre, no sítio mais
 *  visível da app.
 *
 *  E `pior === 0` não queria dizer «zero milissegundos»: queria dizer «ninguém mediu». O
 *  estado «não sei» pintava-se de verde. Agora o RTT vem `null` quando não foi medido, e
 *  «ainda a medir» é um estado próprio, nem verde nem fraco.
 *
 *  A ordem das perguntas é a ordem da gravidade: primeiro «o meu som sai?», depois «o dele
 *  chega?», e só no fim a qualidade do que está a passar.
 */
async function qualidadeDaLigacao() {
  const gente = [...voz.presentes.entries()].filter(([, c]) => c === voz.canal).map(([p]) => p);
  if (!gente.length) return { ok: true, texto: 'Sozinho na sala' };

  const estado = await invoke('qualidade', { peers: gente }).catch(() => null);
  if (!estado || !estado.length) return { ok: false, texto: 'A ligar…' };
  ultimoEstadoDaVoz = estado;

  // 1. O TRANSPORTE RECUSA A MINHA VOZ? É a avaria mais grave e a mais calada: o
  //    `send_datagram` falha, ninguém conta, e a app diz que está tudo bem.
  const recusa = estado.find(e => e.vozFalhados > 20 && e.envS === 0);
  if (recusa) return { ok: false, texto: 'A tua voz não está a sair desta máquina' };

  // 2. SAI DAQUI E NÃO VOLTA NADA? Distinto de «ninguém fala»: só conta se EU estiver a
  //    mandar. Se ninguém manda, não há nada a concluir.
  const aMandar = estado.some(e => e.envS > 0);
  const aReceber = estado.some(e => e.recS > 0);
  if (aMandar && !aReceber) {
    return { ok: false, texto: 'Estás a falar e não chega nada de volta' };
  }

  const relay = estado.some(e => e.relay);
  const medidos = estado.map(e => e.ms).filter(m => typeof m === 'number' && m > 0);
  const sufixo = relay ? ' · por relay' : '';

  // 3. AINDA SEM MEDIDA: nem verde nem fraco. Zero é ausência, não excelência.
  if (!medidos.length) {
    return { ok: null, texto: `Voz conectada · ainda a medir${sufixo}` };
  }
  const pior = Math.max(...medidos);
  return {
    ok: pior < 250 && !relay,
    texto: `Voz conectada · ${Math.round(pior)} ms${sufixo}`,
  };
}

async function desenharRodape() {
  const ligado = !!voz.canal;
  $('#ligacao').hidden = !ligado;

  if (ligado) {
    const s = vista.servidores.find(x => x.id === voz.servidor);
    const canal = s && s.canais.find(c => c.id === voz.canal);
    $('#ligacao-onde').textContent = canal && s ? `${canal.nome} / ${s.nome}` : '—';

    const q = await qualidadeDaLigacao();
    $('#ligacao-estado').textContent = q.texto;
    // `ok === null` é o estado «ainda a medir»: nem verde nem fraco. Pintá-lo de verde era
    // exactamente o que o #171 descreve — dizer «bom» sobre uma coisa que não se sabe.
    $('#ligacao-estado').classList.toggle('is-fraco', q.ok === false);
    $('#ligacao-sinal').classList.toggle('is-fraco', q.ok === false);
    $('#ligacao-sinal').classList.toggle('is-neutro', q.ok === null);

    $('#btn-partilhar').classList.toggle('is-on', !!voz.ecra);
    $('#btn-partilhar').classList.toggle('is-cortado', !!partilhaFalhou);
    $('#btn-partilhar').title = partilhaFalhou
      || partilhaAviso
      || (voz.ecra ? 'Parar de partilhar' : 'Partilhar ecrã');
    $('#btn-partilhar').classList.toggle('is-avisado', !partilhaFalhou && !!partilhaAviso);
    $('#btn-camara').disabled = false;
    $('#btn-camara').classList.toggle('is-on', !!voz.camara);
    $('#btn-camara').classList.toggle('is-cortado', !!camaraFalhou);
    // A razão da falha GANHA ao texto normal. Sem isto, o rodapé — que se redesenha de
    // três em três segundos — apagava a explicação logo a seguir ao clique, e a pessoa
    // ficava com um botão que não faz nada e não diz porquê.
    $('#btn-camara').title = camaraFalhou
      || (voz.camara ? 'Desligar a câmara' : 'Ligar a câmara');
    // O BOTÃO MOSTRA O QUE O DISPOSITIVO FAZ, não o que se lhe pediu (#35, #191).
    const ruidoMostrado = ruidoReal === null ? ruidoSuprimido : ruidoReal;
    $('#btn-ruido').classList.toggle('is-cortado', !ruidoMostrado);
    $('#btn-ruido').classList.toggle('is-avisado',
      ruidoReal !== null && ruidoReal !== ruidoSuprimido);
    $('#btn-ruido').title = ruidoReal !== null && ruidoReal !== ruidoSuprimido
      ? `Pediste supressão de ruído ${ruidoSuprimido ? 'ligada' : 'desligada'} e o teu `
        + `microfone está a fazer o contrário — ele não deixa mudar isto.`
      : (ruidoMostrado ? 'Supressão de ruído ligada' : 'Supressão de ruído desligada');
  }

  const t = voz.micro ? voz.micro.getAudioTracks()[0] : null;
  $('#btn-mic').classList.toggle('is-cortado', (!!t && !t.enabled) || !!vozFalhou);
  // MICROFONE ABERTO E A ENTREGAR ZEROS (#106). Não é um alarme — é uma marca discreta e
  // uma frase no `title`, porque a alternativa legítima («está mesmo calado») existe. O que
  // a distingue é o CHÃO: quinze segundos sem passar de 0,002 não é uma pessoa calada, é um
  // microfone que não capta.
  const mudoDeVerdade = !!t && t.enabled && !!voz.canal
    && acimaDoChaoEm !== null && performance.now() - acimaDoChaoEm > JANELA_DO_PICO;
  $('#btn-mic').classList.toggle('is-avisado', mudoDeVerdade && !vozFalhou);
  // A razão da avaria GANHA ao texto normal, como no botão da câmara.
  $('#btn-mic').title = vozFalhou
    || (mudoDeVerdade
      ? 'O teu microfone está aberto mas não capta nada há 15 s — está silenciado no '
        + 'Windows, tem o botão físico desligado, ou é o dispositivo errado.'
      : (!t ? 'Sem microfone' : (t.enabled ? 'Silenciar microfone' : 'Ligar microfone')));
  $('#btn-surdo').classList.toggle('is-cortado', surdo);
  $('#btn-surdo').classList.toggle('is-avisado', !surdo && !!saidaFalhou);
  $('#btn-surdo').title = saidaFalhou
    || (surdo ? 'Voltar a ouvir' : 'Silenciar tudo');
}

/* --- botões ---------------------------------------------------------------- */

let surdo = false;

/** Onde vive o estado da supressão de ruído (#35, #191). */
const RUIDO = 'bruma.ruido';
/* Era `let ruidoSuprimido = true` — uma variável de sessão, ao contrário dos avisos, do
 * movimento reduzido, da detecção de jogo e da qualidade da partilha, que vão todos ao
 * `localStorage`. Quem a desligava voltava a encontrá-la ligada no arranque seguinte, sem
 * nada que o explicasse. */
let ruidoSuprimido = localStorage.getItem(RUIDO) !== '0';

/** O que o DISPOSITIVO diz que está a fazer — que não é o mesmo que o que se lhe pediu.
 *
 *  `null` enquanto não houver microfone aberto, ou quando o `getSettings()` não devolver o
 *  campo. Não se conclui nada de um campo em falta: dizer «não consigo confirmar» é honesto,
 *  e assumir que obedeceu é exactamente a mentira que o #191 aponta.
 */
let ruidoReal = null;

/** Lê do microfone o que ele está MESMO a fazer, e ajusta o que se mostra (#35, #191).
 *
 *  O `applyConstraints` pode devolver `Ok` e não mudar nada — e o `getUserMedia` pode
 *  simplesmente ignorar metade do que se lhe pediu. Até aqui o painel desenhava o PEDIDO.
 */
function lerRuidoReal() {
  const t = voz.micro ? voz.micro.getAudioTracks()[0] : null;
  // Sem microfone não há nada a afirmar. Sem isto, o painel continuava a dizer no PRESENTE
  // — «o teu microfone está a fazer o contrário» — o que um dispositivo já fechado fazia.
  if (!t || typeof t.getSettings !== 'function') { ruidoReal = null; return; }
  let d = null;
  try { d = t.getSettings(); } catch (e) { d = null; }
  ruidoReal = d && typeof d.noiseSuppression === 'boolean' ? d.noiseSuppression : null;
}

$('#btn-mic').onclick = () => {
  const t = voz.micro ? voz.micro.getAudioTracks()[0] : null;
  if (t) { t.enabled = !t.enabled; desenharVoz(); desenharRodape(); }
};

$('#btn-surdo').onclick = () => {
  // Ficar surdo silencia tudo o que entra E o próprio microfone, como no Discord:
  // não faz sentido continuar a falar para quem não se consegue ouvir a responder.
  surdo = !surdo;
  ajustarTodosOsVolumes();
  const t = voz.micro ? voz.micro.getAudioTracks()[0] : null;
  if (t && surdo) t.enabled = false;
  desenharVoz();
  desenharRodape();
};

/** Põe a supressão de ruído num estado CONCRETO.
 *
 *  Era um `!ruidoSuprimido` cego, e isso partia-se com o próprio #191: o interruptor do
 *  painel mostra o que o DISPOSITIVO faz (`ruidoReal`), mas alternava o que se PEDIU
 *  (`ruidoSuprimido`). Com o dispositivo a ignorar o pedido, os dois divergem — e clicar
 *  numa caixa que mostra «ligado» pedia... ligado outra vez. O interruptor deixava de
 *  responder ao que estava a ver.
 */
async function porRuido(para) {
  ruidoSuprimido = !!para;
  localStorage.setItem(RUIDO, ruidoSuprimido ? '1' : '0');
  const t = voz.micro ? voz.micro.getAudioTracks()[0] : null;
  if (t) {
    try {
      await t.applyConstraints({
        noiseSuppression: ruidoSuprimido,
        echoCancellation: ruidoSuprimido,
        autoGainControl: ruidoSuprimido,
      });
    } catch (e) {
      console.warn('o microfone não aceitou a mudança:', e);
    }
  }
  // Depois de pedir, PERGUNTA-SE. Um `applyConstraints` que não rejeita não é prova de
  // que alguma coisa mudou (#35, #191).
  lerRuidoReal();
  desenharRodape();
  if (aVerAsDefinicoesDaVoz()) mostrarPainel('voz');
}

// O botão da barra alterna a partir do que ELE mostra, que é o real quando ele existe.
$('#btn-ruido').onclick = () =>
  porRuido(!(ruidoReal === null ? ruidoSuprimido : ruidoReal));

$('#btn-desligar').onclick = () => sairDeVoz();
$('#btn-partilhar').onclick = () => alternarEcra();

/** Porque é que a câmara não abriu, se não abriu. Sobrevive aos redesenhos do rodapé. */
let camaraFalhou = null;

$('#btn-camara').onclick = async () => {
  camaraFalhou = null;
  if (voz.camara) {
    pararDeEnviarCamara();
    anunciarEstado();
    desenharVoz();
    desenharRodape();
    return;
  }
  try {
    await comecarAEnviarCamara();
  } catch (e) {
    // Recusar a permissão é uma resposta, não uma avaria. Mas tem de SE VER: um aviso só
    // na consola é o mesmo que não haver aviso nenhum para quem carregou no botão.
    console.warn('a câmara não abriu:', e);
    camaraFalhou = `A câmara não abriu: ${e && e.message ? e.message : e}`;
    desenharRodape();
    return;
  }
  anunciarEstado();
  desenharVoz();
  desenharRodape();
};

// A ligação muda de qualidade sozinha; o rodapé acompanha.
setInterval(() => { if (voz.canal) desenharRodape(); }, 3000);

/* ==========================================================================
   Vista de chamada: quem está, quem fala, e quem transmite.
   ========================================================================== */

/** Deteção de fala.
 *
 *  Não se pergunta ao WebRTC se alguém está a falar — ele não sabe. Mede-se a energia
 *  do áudio com a Web Audio API, e usa-se histerese: entra em "a falar" acima de um
 *  limiar e só sai abaixo de outro mais baixo. Sem isso o anel verde pisca em cada
 *  pausa entre sílabas, que é pior do que não o ter.
 */
const LIMIAR_ENTRA = 0.045;
const LIMIAR_SAI = 0.022;

/** O anel verde de quem fala, medido no som que se acabou de descodificar.
 *
 *  Antes havia um analisador da Web Audio por pessoa, ligado ao fluxo do WebRTC. Agora as
 *  amostras já passam por aqui a caminho dos altifalantes — medi-las outra vez num
 *  analisador em paralelo seria fazer o mesmo trabalho duas vezes.
 *
 *  A histerese fica: entra-se em "a falar" acima de um limiar e só se sai abaixo de outro
 *  mais baixo. Sem isso o anel pisca em cada pausa entre sílabas, que é pior do que não o
 *  ter.
 */
function medirNasAmostras(chave, amostras) {
  let soma = 0;
  for (let i = 0; i < amostras.length; i++) soma += amostras[i] * amostras[i];
  const rms = Math.sqrt(soma / Math.max(1, amostras.length));

  const estava = voz.falando.has(chave);
  const agora = estava ? rms > LIMIAR_SAI : rms > LIMIAR_ENTRA;
  if (agora === estava) return;

  if (agora) voz.falando.add(chave); else voz.falando.delete(chave);
  document.querySelectorAll(`[data-chave="${chave}"]`).forEach(el => {
    el.classList.toggle('a-falar', agora);
  });
}

function vigiarAudio(chave, stream) {
  pararDeVigiar(chave);
  if (!stream || !stream.getAudioTracks().length) return;
  try {
    if (!voz.audioCtx) voz.audioCtx = new AudioContext();
    if (voz.audioCtx.state === 'suspended') voz.audioCtx.resume();
    const fonte = voz.audioCtx.createMediaStreamSource(stream);
    const an = voz.audioCtx.createAnalyser();
    an.fftSize = 512;
    an.smoothingTimeConstant = 0.4;
    fonte.connect(an);
    voz.analisadores.set(chave, { an, fonte, dados: new Float32Array(an.fftSize) });
  } catch (e) {
    console.warn('não consegui vigiar o áudio de', chave, e);
  }
}

function pararDeVigiar(chave) {
  const a = voz.analisadores.get(chave);
  if (!a) return;
  try { a.fonte.disconnect(); } catch (e) { /* já desligado */ }
  voz.analisadores.delete(chave);
  voz.falando.delete(chave);
}

function medirFala() {
  if (!voz.canal || !voz.analisadores.size) return;
  for (const [chave, a] of voz.analisadores) {
    a.an.getFloatTimeDomainData(a.dados);
    let soma = 0;
    for (let i = 0; i < a.dados.length; i++) soma += a.dados[i] * a.dados[i];
    const rms = Math.sqrt(soma / a.dados.length);

    // O microfone silenciado nunca "fala", por mais barulho que haja na sala.
    const proprioSilenciado =
      chave === voz.eu && (!voz.micro || !voz.micro.getAudioTracks()[0]?.enabled);

    // O PRÓPRIO PASSOU DO CHÃO? (#106) Só se escreve quando SIM. Escrever também no caso
    // do «não» — que era o que a versão anterior fazia, ao rodar a janela do pico — punha o
    // escritor a apagar de dois em dois segundos a prova que o leitor precisava.
    if (chave === voz.eu && rms >= CHAO_DO_MICRO) acimaDoChaoEm = performance.now();

    // O ANEL DO PRÓPRIO EXIGE AS DUAS COISAS (#36): energia no microfone E um pedaço a
    // sair. Quando há energia e não sai nada é isso mesmo que se quer mostrar — o anel
    // apagado, e a razão no botão do microfone. Só se aplica a mim: dos outros, o que
    // chega é a prova de que saiu.
    const naoSai = chave === voz.eu
      && performance.now() - ultimoPedacoSaiu > SAIDA_RECENTE_MS;

    const estava = voz.falando.has(chave);
    const agora = (proprioSilenciado || naoSai)
      ? false : (estava ? rms > LIMIAR_SAI : rms > LIMIAR_ENTRA);
    if (agora === estava) continue;

    if (agora) voz.falando.add(chave); else voz.falando.delete(chave);
    // Mexe-se nas classes diretamente: redesenhar tudo dez vezes por segundo daria
    // um piscar constante e mataria a lista de mensagens.
    document.querySelectorAll(`[data-chave="${chave}"]`).forEach(el => {
      el.classList.toggle('a-falar', agora);
    });
  }
}
setInterval(medirFala, 120);

// A janela muda de tamanho, os painéis acompanham.
addEventListener('resize', () => { if (voz.canal) desenharVoz(); });

/* --- estado partilhado entre peers ----------------------------------------- */

/** Diz a toda a gente na sala o que estou a enviar.
 *
 *  Quem recebe um fluxo de vídeo não consegue saber, do lado de lá, se aquilo é um ecrã ou
 *  uma câmara — os bytes são os mesmos. Portanto quem envia é que tem de contar.
 */
/** O que esta versão sabe falar.
 *
 *  # Porque é que isto teve de existir
 *
 *  Duas mudanças de hoje partem o entendimento com quem ainda não actualizou, e as duas
 *  falham em SILÊNCIO — que é a pior maneira de falhar:
 *
 *  1. A **câmara** vai pelo mesmo transporte do ecrã, distinguida por um campo novo no
 *     cabeçalho. O serde ignora campos que não conhece, portanto uma versão anterior aceita
 *     o cabeçalho e mete os frames da câmara no caminho do ECRÃ. Não estoira: mostra lixo.
 *  2. O **som na partilha de ecrã** vem como segunda faixa no contentor, e uma versão
 *     anterior só sabe traduzir uma. Resultado: ecrã preto, sem nenhuma mensagem de erro.
 *
 *  Não havia forma de as duas pontas saberem com quem estavam a falar. Passa a haver: cada
 *  lado diz um número, e quem não o diz é antigo. `1` é implícito — é o que as versões que
 *  nunca souberam disto valem.
 */
const PROTOCOLO = 2;

/** "1440p 60FPS" — como o Discord o escreve, e a partir dos números que valem mesmo.
 *
 *  Mostra-se sempre o MEDIDO e não o pedido, porque é o medido que a outra pessoa recebe.
 *  Com "Nativa" o pedido é zero, e mesmo com um número o Rust arredonda para blocos pares.
 */
function rotuloDaQualidade() {
  const q = voz.qualidadeEmUso || qualidadeEfetiva();
  const alt = (voz.ecraTamanho && voz.ecraTamanho.altura) || q.altura;
  return `${alt ? alt + 'p' : 'Nativa'} ${q.fps}FPS`;
}

/** Porque é que o número não é o que a pessoa escolheu, quando não é.
 *
 *  A resolução do menu é um TETO, não um alvo: nunca amplia. Quem partilha uma janela de
 *  1050 de altura e escolheu 1440p vê "1050p" e fica sem perceber se a escolha funcionou.
 *  Devolve `null` quando não há nada a explicar.
 */
function porqueEstaResolucao() {
  const q = voz.qualidadeEmUso;
  if (!q || !voz.ecraTamanho) return null;
  const real = voz.ecraTamanho.altura;
  if (!q.altura) return `a fonte tem ${real} de altura e vai como está (escolheste Nativa)`;
  if (real < q.altura) {
    return `escolheste ${q.altura}p, mas a fonte só tem ${real} de altura — a escolha é um`
      + ` limite, não aumenta o que não existe`;
  }
  // Aqui só se sabe o tamanho de SAÍDA, não o da fonte — portanto não se pode afirmar que
  // houve redução. Dizer "foi reduzida" quando a fonte já era desse tamanho seria inventar.
  // Uma frase vaga e sempre verdadeira vale mais do que uma precisa e às vezes falsa.
  return `vai a ${voz.ecraTamanho.largura}x${real}, dentro do limite de ${q.altura}p que`
    + ` escolheste`;
}

function anunciarEstado() {
  for (const [peer, c] of voz.presentes) {
    if (c !== voz.canal) continue;
    sinalizar(peer, {
      tipo: 'estado', ecra: !!voz.ecra, camara: !!voz.camara, v: PROTOCOLO,
      // Quem transmite é o único que sabe estas duas coisas. Sem as dizer, quem assiste
      // teria de as adivinhar — e a barra mostraria um palpite em vez de um facto.
      qualidade: voz.ecra ? rotuloDaQualidade() : null,
      espectadores: voz.ecra ? voz.aSerVistoPor.size : 0,
    });
  }
}

/* --- assistir a uma transmissão -------------------------------------------- */

function deixarDeVerOMeu() {
  if (!voz.vejoMeuEcra) return;
  invoke('parar_de_ver_meu_ecra').catch(() => {});
  voz.vejoMeuEcra.fechar();
  voz.vejoMeuEcra = null;
}

function assistir(peer) {
  if (voz.aVer && voz.aVer !== peer && voz.aVer !== voz.eu) {
    sinalizar(voz.aVer, { tipo: 'assistir', ligado: false });
  }
  if (voz.aVer === voz.eu && peer !== voz.eu) deixarDeVerOMeu();
  voz.aVer = peer;
  if (peer === voz.eu) {
    // O fluxo nasce AGORA, e só agora é que o Rust abre a torneira — reenviando o
    // princípio da transmissão primeiro, para o <video> saber o que vai receber.
    voz.vejoMeuEcra = fluxoDePedacos();
    desenharVoz();                       // o <video> entra no DOM antes dos pedaços
    invoke('ver_meu_ecra').catch(() => {});
    return;
  }
  // Quem transmite tem de saber que estou a ver: é essa lista que decide o que sai da
  // máquina dele. Sem isto o ecrã era codificado para ninguém.
  sinalizar(peer, { tipo: 'assistir', ligado: true });
  desenharVoz();
}

function pararDeAssistir() {
  if (voz.aVer === voz.eu) deixarDeVerOMeu();
  else if (voz.aVer) sinalizar(voz.aVer, { tipo: 'assistir', ligado: false });
  voz.aVer = null;
  desenharVoz();
}

/* ---------- arranque ---------- */


(async () => {
  voz.eu = await invoke('meu_endereco').catch(() => null);
  await desenharTudo();
  // A fotografia do que JÁ estava por ler, agora e não na primeira mensagem que chegar.
  // Estava a ser tirada dentro do `talvezAvisar`, que só corre no `servidor-mudou` — logo a
  // primeira mensagem de cada sessão servia de base e nunca avisava. Numa app de mensagens,
  // a primeira mensagem de uma sessão é exactamente aquela de que se quer saber.
  fotografarPorLer();
  if (!vista.nome) {
    abrir('veu-bemvindo');
    $('#in-nome').focus();
  }
  (async () => {
    // Se a última actualização morreu a meio, é a primeira coisa a dizer (#121). O
    // instalador deixa um carimbo; a app lê-o uma vez e apaga-o. Sem isto, o UAC recusado
    // acabava com a app a reabrir na versão antiga sem uma palavra.
    const incompleta = await invoke('actualizacao_incompleta').catch(() => null);
    const houve = await procurarAtualizacao(true);
    if (incompleta) {
      if (houve === 'ha') {
        // A faixa já está montada pela procura: junta-se o contexto à frente.
        $('#texto-update').textContent = `${incompleta} ${$('#texto-update').textContent}`;
      } else {
        // Sem rede, ou já não há nada para instalar: diz-se o facto na mesma, e o botão
        // fica a repetir a procura — é o gesto certo quando a rede voltar.
        $('#texto-update').textContent = incompleta;
        $('#faixa-update').hidden = false;
        $('#adiar-update').onclick = () => { $('#faixa-update').hidden = true; };
        $('#btn-update').onclick = () => procurarAtualizacao();
      }
    }
  })();
  // E volta-se a procurar de tempos a tempos (#62): quem deixa a app aberta dias a fio
  // nunca via o aviso, porque a procura só acontecia no arranque. Quatro horas — não é
  // urgente, e o adiamento por versão garante que isto nunca vira insistência.
  setInterval(() => procurarAtualizacao(true), 4 * 60 * 60 * 1000);
  verJogo();
  desenharRodape();
})();

/* ---------- o que esta webview consegue descodificar ----------------------- */

/* O ecrã vai passar a chegar como H.264 nosso, descodificado aqui pelo WebCodecs em vez
   de vir por WebRTC. Isso depende da versão da WebView2 que cada pessoa tem instalada, e
   a aceleração por hardware depende ainda da placa — não é coisa para se assumir. */
(async () => {
  const diz = linha => invoke('capacidades', { linha }).catch(() => {});
  if (typeof VideoDecoder === 'undefined') {
    return diz('sem WebCodecs — esta webview não descodifica o ecrã nativo');
  }
  const perfil = {
    codec: 'avc1.640028',      // H.264 High, nível 4.0 — chega para 1080p60
    codedWidth: 1920,
    codedHeight: 1080,
    optimizeForLatency: true,
  };
  // Cuidado com o que isto responde: o `isConfigSupported` diz que a configuração é
  // ACEITE, não que a descodificação vá parar ao hardware — o `prefer-hardware` é uma
  // dica, e a config devolvida limita-se a repetir a preferência pedida. Quem responde a
  // "usou mesmo o hardware" é a utilização do descodificador da GPU, com stream a sério.
  const aceita = async preferencia => {
    try {
      const r = await VideoDecoder.isConfigSupported({ ...perfil, hardwareAcceleration: preferencia });
      return r.supported ? 'aceite' : 'recusado';
    } catch (e) { return `erro: ${e.name}`; }
  };
  diz(`WebCodecs presente · config H.264 1080p: prefere-hardware=${await aceita('prefer-hardware')}`
    + ` prefere-software=${await aceita('prefer-software')} indiferente=${await aceita('no-preference')}`);

  // A voz vai pelo mesmo caminho do ecrã ou continua a precisar de configuração à mão?
  // Depende destas três, e nenhuma se pode assumir.
  const audio = { codec: 'opus', sampleRate: 48000, numberOfChannels: 1, bitrate: 24000 };
  const pergunta = async (classe, nome) => {
    if (typeof classe === 'undefined') return 'não existe';
    try {
      const r = await classe.isConfigSupported(audio);
      return r.supported ? 'aceite' : 'recusado';
    } catch (e) { return `erro: ${e.name}`; }
  };
  diz(`áudio · AudioEncoder=${await pergunta(window.AudioEncoder)}`
    + ` AudioDecoder=${await pergunta(window.AudioDecoder)}`
    + ` MediaStreamTrackProcessor=${typeof window.MediaStreamTrackProcessor === 'undefined' ? 'não existe' : 'existe'}`
    + ` AudioWorklet=${typeof AudioWorkletNode === 'undefined' ? 'não existe' : 'existe'}`);
})();

/* ---------- autoteste do ECO ------------------------------------------------ */

/* A pergunta: quando o Bruma partilha o som do sistema, ele apanha a SUA PRÓPRIA voz?

   Se apanhasse, a voz das outras pessoas na chamada — que sai pelas colunas por ordem do
   Bruma — voltava a entrar na partilha e era reenviada. Quem estivesse do outro lado
   ouvia-se a si próprio, com o atraso do caminho todo.

   Mede-se por diferença, que é a única forma honesta: primeiro com a app calada, depois
   com ela a tocar um tom bem alto por si mesma. Se a captura nos exclui, os dois números
   são praticamente iguais — e se não exclui, o segundo dispara.

   Mede-se por DIFERENÇA e não em absoluto porque a máquina pode ter outra coisa a tocar,
   e essa é para ser captada: é justamente o que a partilha de ecrã deve levar. */
(async () => {
  if (!window.__TAURI__) return;
  if (!(await invoke('autoteste_pedido').catch(() => 0))) return;
  const diz = linha => invoke('capacidades', { linha }).catch(() => {});

  const calado = await invoke('medir_som', { ms: 1200 }).catch(e => ({ erro: String(e) }));
  if (calado.erro) return diz(`autoteste eco: não consegui medir (${calado.erro})`);

  // Um tom nosso, alto, pelo mesmo caminho por onde sai a voz das outras pessoas.
  const ctx = contextoDeAudio();
  const osc = ctx.createOscillator();
  const vol = ctx.createGain();
  osc.frequency.value = 440;
  vol.gain.value = 0.35;
  osc.connect(vol); vol.connect(ctx.destination);
  osc.start();
  const aTocar = await invoke('medir_som', { ms: 1200 }).catch(() => null);
  osc.stop();
  try { osc.disconnect(); vol.disconnect(); } catch (e) { /* já solto */ }
  if (!aTocar) return diz('autoteste eco: a segunda medição falhou');

  // Com o tom a 0.35 de ganho, se ele entrasse na captura o rms subia MUITO. O limiar é
  // generoso de propósito: o que se quer distinguir é "não entrou" de "entrou todo".
  const subiu = aTocar.rms - calado.rms;
  const passou = aTocar.semEco ? subiu < 500 : true;
  diz(`autoteste eco: eu=${aTocar.eu} a tocar agora: ${(aTocar.quem || []).join(', ') || 'ninguem'}`);
  diz(`autoteste eco: eu=${aTocar.eu} | a tocar agora: ${(aTocar.quem || []).join(', ') || 'ninguem'}`);
  diz(`autoteste eco: semEco=${aTocar.semEco}`
    + ` rms calado=${calado.rms.toFixed(0)} a tocar=${aTocar.rms.toFixed(0)}`
    + ` (subiu ${subiu.toFixed(0)}) -> ${aTocar.semEco ? (passou ? 'EXCLUIDO' : 'FALHOU: o nosso som entrou') : 'sem exclusao neste Windows'}`);
})();

/* ---------- autoteste da câmara -------------------------------------------- */

/* Prova o caminho da câmara SEM rede e SEM segunda máquina: abre a câmara (ou, se não
   houver nenhuma, uma tela desenhada por nós), codifica, e mete os pedaços num
   descodificador de verdade. É a mesma ideia do teste de par — verificar cada metade não
   verifica o meio — aplicada ao codec: o que interessa não é "o codificador aceitou a
   configuração", é "saíram bytes que um descodificador conseguiu transformar em imagem".

   O caso sem câmara não é um atalho: prova a codificação e a descodificação em qualquer
   máquina, incluindo as do CI, onde câmara não há nenhuma. */
(async () => {
  if (!window.__TAURI__) return;
  if (!(await invoke('autoteste_pedido').catch(() => 0))) return;
  const diz = linha => invoke('capacidades', { linha }).catch(() => {});

  diz('autoteste câmara: a começar');
  if (typeof VideoEncoder === 'undefined' || typeof VideoDecoder === 'undefined') {
    return diz('autoteste câmara: esta webview não traz VideoEncoder/VideoDecoder');
  }

  /** Um pedido que DESISTE. Sem isto, uma permissão que nunca é respondida deixa o teste
   *  pendurado para sempre — e um teste pendurado parece-se com um teste que não corre. */
  const comPrazo = (promessa, ms, oque) => Promise.race([
    promessa,
    new Promise((_, mal) => setTimeout(() => mal(new Error(`${oque} não respondeu em ${ms} ms`)), ms)),
  ]);

  let fonte = 'câmara';
  let faixa = null;
  let parar = () => {};
  try {
    const dispositivos = await comPrazo(
      navigator.mediaDevices.enumerateDevices(), 4000, 'enumerateDevices');
    const camaras = dispositivos.filter(d => d.kind === 'videoinput');
    diz(`autoteste câmara: ${camaras.length} dispositivo(s): `
      + camaras.map(c => `"${c.label || 'sem nome'}"`).join(', '));
    if (!camaras.length) throw new Error('nenhuma');
    // Duas tentativas, e a diferença entre elas é diagnóstico: se a apertada falha e a
    // solta passa, o problema é a resolução pedida; se falham as duas, é o dispositivo.
    let stream;
    try {
      stream = await comPrazo(navigator.mediaDevices.getUserMedia({
        video: { width: CAM_LARGURA, height: CAM_ALTURA, frameRate: CAM_IPS }, audio: false,
      }), 6000, 'getUserMedia');
    } catch (apertado) {
      diz(`autoteste câmara: com 640x360 falhou (${apertado.message}); a tentar sem exigências`);
      stream = await comPrazo(
        navigator.mediaDevices.getUserMedia({ video: true, audio: false }), 6000, 'getUserMedia');
    }
    faixa = stream.getVideoTracks()[0];
    parar = () => stream.getTracks().forEach(t => t.stop());
    diz(`autoteste câmara: ${camaras.length} dispositivo(s); a usar "${camaras[0].label || 'sem nome'}"`);
  } catch (e) {
    // Sem câmara desenha-se uma: um quadrado que ANDA, porque uma imagem parada
    // comprimiria para quase nada e não provaria que o codificador está a trabalhar.
    fonte = 'tela desenhada';
    const tela = document.createElement('canvas');
    tela.width = CAM_LARGURA; tela.height = CAM_ALTURA;
    const pincel = tela.getContext('2d');
    let x = 0;
    const pintar = () => {
      pincel.fillStyle = '#101418';
      pincel.fillRect(0, 0, tela.width, tela.height);
      pincel.fillStyle = '#7fd4c1';
      pincel.fillRect(x % (tela.width - 60), 40 + (x % 200), 60, 60);
      x += 11;
    };
    const relogio = setInterval(pintar, 1000 / CAM_IPS);
    pintar();
    const stream = tela.captureStream(CAM_IPS);
    faixa = stream.getVideoTracks()[0];
    parar = () => { clearInterval(relogio); stream.getTracks().forEach(t => t.stop()); };
    diz(`autoteste câmara: sem dispositivo (${e.message}); a usar uma tela desenhada`);
  }

  let codificados = 0, bytes = 0, chaves = 0, desenhados = 0, erros = 0;
  let larguraVista = 0, alturaVista = 0;

  const descodificador = new VideoDecoder({
    output: q => {
      desenhados += 1;
      larguraVista = q.displayWidth; alturaVista = q.displayHeight;
      q.close();
    },
    error: e => { erros += 1; console.warn('autoteste câmara, descodificador:', e); },
  });
  descodificador.configure({ codec: 'avc1.42001f', optimizeForLatency: true });

  let esperaChave = true;
  const codificador = new VideoEncoder({
    output: pedaco => {
      codificados += 1;
      const b = new Uint8Array(pedaco.byteLength);
      pedaco.copyTo(b);
      bytes += b.length;
      // O MESMO teste que o caminho real usa, e de propósito: se o `temSPS` estiver
      // errado, este autoteste tem de falhar aqui e não deixar o erro para a chamada.
      const completo = temSPS(b);
      if (completo) chaves += 1;
      if (esperaChave) {
        if (!completo) return;
        esperaChave = false;
      }
      try {
        descodificador.decode(new EncodedVideoChunk({
          type: completo ? 'key' : 'delta',
          timestamp: codificados * 1000,
          data: b,
        }));
      } catch (e) { erros += 1; }
    },
    error: e => { erros += 1; console.warn('autoteste câmara, codificador:', e); },
  });
  codificador.configure({
    codec: 'avc1.42001f', width: CAM_LARGURA, height: CAM_ALTURA,
    framerate: CAM_IPS, bitrate: CAM_DEBITO, latencyMode: 'realtime',
    avc: { format: 'annexb' },
  });

  const leitor = new MediaStreamTrackProcessor({ track: faixa }).readable.getReader();
  const ate = performance.now() + 4000;
  let lidos = 0, ultimaChave = 0;
  while (performance.now() < ate) {
    const { value, done } = await leitor.read().catch(() => ({ done: true }));
    if (done) break;
    lidos += 1;
    const agora = performance.now();
    const chave = agora - ultimaChave >= CAM_CHAVE_MS;
    if (chave) ultimaChave = agora;
    try { codificador.encode(value, { keyFrame: chave }); } catch (e) { erros += 1; }
    value.close();
  }
  try { await codificador.flush(); } catch (e) { /* segue */ }
  await new Promise(r => setTimeout(r, 400));
  try { leitor.cancel(); } catch (e) { /* idem */ }
  parar();
  try { codificador.close(); } catch (e) { /* idem */ }
  try { descodificador.close(); } catch (e) { /* idem */ }

  diz(`autoteste câmara (${fonte}): ${lidos} lidos, ${codificados} codificados`
    + ` (${(bytes / 1024).toFixed(0)} KB, ${chaves} completos),`
    + ` ${desenhados} DESCODIFICADOS a ${larguraVista}x${alturaVista}, ${erros} erros`);
})();

/* ---------- autoteste da partilha de ecrã ---------------------------------- */

/* Corre só com `bruma --autoteste`. Parte do princípio de que nada funciona e vai
   verificando: os pedaços chegam? o vídeo aceita-os? tem dimensões? o tempo anda?
   Cada uma dessas perguntas falha de maneira diferente e em sítios diferentes. */
(async () => {
  if (!window.__TAURI__) return;
  const segundos = await invoke('autoteste_pedido').catch(() => 0);
  if (!segundos) return;

  const diz = linha => invoke('capacidades', { linha }).catch(() => {});
  const fluxo = fluxoDePedacos();
  document.body.append(fluxo.el);          // o MediaSource só anda com o elemento no DOM
  fluxo.el.style.cssText = 'position:fixed;width:2px;height:2px;opacity:0;pointer-events:none';

  let pedacos = 0, bytes = 0;
  const inteiro = [];   // o mesmo vídeo, para o provar por um caminho que não é o MSE
  const canal = new window.__TAURI__.core.Channel();
  canal.onmessage = p => {
    const b = p instanceof ArrayBuffer ? new Uint8Array(p) : new Uint8Array(p);
    pedacos += 1; bytes += b.length;
    if (b.length && b[0] === ETIQUETA_BYTES) inteiro.push(b.subarray(1));
    fluxo.empurrar(b);
  };

  try {
    const r = await invoke('comecar_a_partilhar',
      { servidor: 'autoteste', canalVoz: 'autoteste',
        fonte: await invoke('autoteste_fonte').catch(() => 'ecra:1'),
        altura: await invoke('autoteste_altura').catch(() => 720),
        fps: await invoke('autoteste_fps').catch(() => 30),
        debito: 0, comSom: true, saida: canal });
    await invoke('ver_meu_ecra');   // a pré-visualização é gated: sem isto nada chega
    const fontes = await invoke('fontes_de_partilha').catch(() => []);
    const comImagem = fontes.filter(f => f.miniatura && f.miniatura.length > 2000).length;
    diz(`autoteste fontes: ${fontes.length} no total, ${comImagem} com miniatura`
      + ` (${fontes.slice(0, 4).map(f => f.tipo + ':' + f.titulo.slice(0, 18)).join(' | ')})`);
    diz(`autoteste: a captar a ${r.largura}x${r.altura}`);
  } catch (e) {
    return diz(`autoteste FALHOU a arrancar: ${e}`);
  }

  await new Promise(r => setTimeout(r, segundos * 1000));
  await invoke('parar_de_partilhar').catch(() => {});

  const v = fluxo.el;
  const intervalos = [];
  for (let i = 0; i < v.buffered.length; i++) {
    intervalos.push(`${v.buffered.start(i).toFixed(2)}-${v.buffered.end(i).toFixed(2)}`);
  }
  diz(`autoteste: ${pedacos} pedaços, ${(bytes / 1e6).toFixed(1)} MB`
    + ` | vídeo ${v.videoWidth}x${v.videoHeight}, readyState=${v.readyState}`
    + ` | descodificados=${v.getVideoPlaybackQuality ? v.getVideoPlaybackQuality().totalVideoFrames : '?'}`
    // "sem erro" não prova que o som toca: prova que ninguém se queixou. Estes bytes são o
    // navegador a dizer que DESCODIFICOU som — é a diferença entre a faixa existir e a
    // faixa funcionar, e foi por não a medir que o vídeo já passou por bom estando parado.
    + ` | som descodificado=${v.webkitAudioDecodedByteCount ?? '?'} bytes`
    + ` | bufferizado=[${intervalos.join(', ')}] t=${v.currentTime.toFixed(2)}`
    + ` | erro=${v.error ? v.error.code : 'nenhum'}`
    + (v.error && v.error.message ? ` "${v.error.message}"` : ''));
  fluxo.fechar();

  /* E a segunda prova, por fora do MSE: os mesmos bytes num <video> comum. Se este toca
     e o de cima não, o ficheiro está bom e o problema é do dialeto que o MSE exige; se
     nenhum toca, o problema está antes, no que estamos a produzir. Sem separar as duas
     coisas, o passo seguinte é adivinhar. */
  // ---- a voz, com o circuito fechado aqui mesmo ----------------------------
  // Não dá para provar a voz sozinho de um lado ao outro da rede, mas dá para provar a
  // metade que vive aqui: o microfone é codificado em Opus e descodificado a seguir, e
  // conta-se o que entrou e o que saiu. Se isto não fechar, não vale a pena procurar na
  // rede. O transporte tem prova própria, no `cargo test` do módulo da rede.
  try {
    const mic = await navigator.mediaDevices.getUserMedia({ audio: true });
    let codificados = 0, descodificados = 0, amostras = 0, energia = 0;

    const dec = new AudioDecoder({
      output: som => {
        descodificados += 1;
        amostras += som.numberOfFrames;
        const f = new Float32Array(som.numberOfFrames);
        try {
          som.copyTo(f, { planeIndex: 0, format: 'f32-planar' });
          for (let i = 0; i < f.length; i++) energia += f[i] * f[i];
        } catch (e) { /* formato inesperado */ }
        som.close();
      },
      error: e => console.warn('descodificador:', e),
    });
    dec.configure({ codec: 'opus', sampleRate: 48000, numberOfChannels: 1 });

    const enc = new AudioEncoder({
      output: pedaco => {
        codificados += 1;
        const b = new Uint8Array(pedaco.byteLength);
        pedaco.copyTo(b);
        try {
          dec.decode(new EncodedAudioChunk({
            type: 'key', timestamp: pedaco.timestamp, data: b,
          }));
        } catch (e) { /* segue */ }
      },
      error: e => console.warn('codificador:', e),
    });
    enc.configure({
      codec: 'opus', sampleRate: 48000, numberOfChannels: 1,
      bitrate: 24000, opus: { frameDuration: 20000 },
    });

    const leitor = new MediaStreamTrackProcessor({ track: mic.getAudioTracks()[0] })
      .readable.getReader();
    const fim = Date.now() + 3000;
    while (Date.now() < fim) {
      const { value, done } = await leitor.read();
      if (done) break;
      enc.encode(value);
      value.close();
    }
    await enc.flush();
    await dec.flush();
    leitor.cancel();
    mic.getTracks().forEach(t => t.stop());

    const rms = amostras ? Math.sqrt(energia / amostras) : 0;
    diz(`autoteste voz: ${codificados} pedaços codificados, ${descodificados} descodificados`
      + ` (${(amostras / 48000).toFixed(1)}s de som, rms ${rms.toFixed(4)})`);
  } catch (e) {
    diz(`autoteste voz FALHOU: ${e.name} — ${e.message}`);
  }

  const simples = document.createElement('video');
  simples.muted = true;
  simples.style.cssText = 'position:fixed;width:2px;height:2px;opacity:0';
  document.body.append(simples);
  simples.src = URL.createObjectURL(new Blob(inteiro, { type: 'video/mp4' }));
  await new Promise(r => {
    simples.onloadeddata = r;
    simples.onerror = r;
    setTimeout(r, 5000);
  });
  diz(`autoteste (sem MSE, ficheiro inteiro): ${simples.videoWidth}x${simples.videoHeight}`
    + ` readyState=${simples.readyState} duração=${simples.duration}`
    + ` erro=${simples.error ? simples.error.code : 'nenhum'}`
    + (simples.error && simples.error.message ? ` "${simples.error.message}"` : ''));
  simples.remove();
  v.remove();
})();

/* ---------- autoteste de par: duas instâncias a falar ---------------------- */

/* A voz tem duas metades que se provam sozinhas — o codec e o transporte — e uma que não:
   a do meio. Quem está na sala, o datagrama a sair para a pessoa certa, e o pedaço a
   chegar ao descodificador do outro lado. Isso só se vê com duas instâncias.

     bruma --par              cria o servidor e escreve o convite
     bruma --par=<convite>    entra, junta-se à sala e conta o que ouviu

   Cada uma com o seu BRUMA_DADOS, senão partilham a identidade e não são duas pessoas. */
(async () => {
  if (!window.__TAURI__) return;
  const modo = await invoke('autoteste_par').catch(() => null);
  if (modo === null || modo === undefined) return;

  const diz = linha => invoke('capacidades', { linha }).catch(() => {});
  const esperar = ms => new Promise(r => setTimeout(r, ms));

  try {
    // Um nome, para a resolucao de nomes ser mesmo exercitada. Sem isto as duas instancias
    // ficam sem nome, tudo aparece como chave truncada, e o teste passava sem provar nada
    // sobre o caminho que leva o nome de uma pessoa ate a mensagem dela.
    await invoke('definir_nome', { nome: modo === '' ? 'Anfitriao' : 'Convidado' })
      .catch(e => diz(`par nao consegui dar-me um nome: ${e}`));

    let servidorId;
    if (modo === '') {
      servidorId = await invoke('criar_servidor', { nome: 'par' });
      await invoke('criar_canal', { servidor: servidorId, nome: 'sala', tipo: 'voz' });
      await invoke('criar_canal', { servidor: servidorId, nome: 'geral', tipo: 'texto' });

      // Escrever ANTES de o convidado existir. É a promessa central do projeto — que nada
      // morre por o outro estar offline — e ninguém guardou isto num servidor: fica aqui,
      // à espera de alguém a quem o dar.
      await desenharTudo();
      const texto = vista.servidores.find(x => x.id === servidorId)
        .canais.find(c => c.tipo === 'texto');
      for (let i = 1; i <= 5; i++) {
        await invoke('enviar', { servidor: servidorId, canal: texto.id, texto: `antes ${i}` });
      }
      diz('par ANFITRIAO escreveu 5 mensagens antes de existir convidado');

      // E uma DURANTE o sync, que e a janela onde uma mensagem se perdia: entre a
      // fotografia do log e o momento em que a sessao comecava a ouvir o canal das
      // novidades. Escrita mal o par liga, com o sync propositadamente lento.
      listen('peer-ligado', () => {
        invoke('enviar', { servidor: servidorId, canal: texto.id, texto: 'durante o sync' })
          .then(() => diz('par ANFITRIAO escreveu uma mensagem DURANTE o sync'))
          .catch(e => diz(`par ANFITRIAO nao conseguiu escrever durante o sync: ${e}`));
      });

      const convite = await invoke('criar_convite', { servidor: servidorId });
      diz(`par ANFITRIAO convite=${convite}`);
    } else {
      servidorId = await invoke('entrar_com_convite', { codigo: modo });
      diz('par CONVIDADO entrou');

      // O histórico que existia antes de eu existir tem de chegar cá.
      let msgs = [];
      for (let i = 0; i < 30 && msgs.length < 5; i++) {
        await esperar(500);
        await desenharTudo();
        const srv = vista.servidores.find(x => x.id === servidorId);
        const t = srv && srv.canais.find(c => c.tipo === 'texto');
        if (t) msgs = await invoke('mensagens', { servidor: servidorId, canal: t.id }).catch(() => []);
      }
      // A que foi escrita enquanto o sync ainda corria tem de chegar tambem.
      let durante = false;
      for (let i = 0; i < 24 && !durante; i++) {
        await esperar(500);
        const srv2 = vista.servidores.find(x => x.id === servidorId);
        const t2 = srv2 && srv2.canais.find(c => c.tipo === 'texto');
        if (t2) {
          const todas = await invoke('mensagens', { servidor: servidorId, canal: t2.id }).catch(() => []);
          durante = todas.some(m => m.texto === 'durante o sync');
        }
        await desenharTudo();
      }
      diz(`par CONVIDADO recebeu a mensagem escrita DURANTE o sync: ${durante}`);

      diz(`par CONVIDADO recebeu ${msgs.length}/5 mensagens escritas antes de ele entrar`
        + (msgs.length ? ` (primeira: "${msgs[0].texto}", última: "${msgs[msgs.length - 1].texto}")` : ''));
    }

    // Esperar que o canal de voz apareça: o convidado só o conhece depois de sincronizar.
    let canal = null;
    for (let i = 0; i < 40 && !canal; i++) {
      await desenharTudo();
      const srv = vista.servidores.find(x => x.id === servidorId);
      canal = srv && srv.canais.find(c => c.tipo === 'voz');
      if (!canal) await esperar(500);
    }
    if (!canal) return diz('par FALHOU: o canal de voz nunca apareceu');

    servidorAtual = servidorId;
    canalAtual = canal.id;
    await entrarEmVoz(servidorId, canal.id);
    diz(`par entrou na sala (microfone=${voz.micro ? 'sim' : 'não'})`);

    // As DUAS instâncias ligam a câmara. Ao contrário do ecrã, que é um de cada vez, as
    // câmaras são simultâneas — e é precisamente aí que mora o bug que uma instância
    // sozinha nunca mostra: cada lado tem de descodificar o outro enquanto codifica o seu.
    try {
      await comecarAEnviarCamara(camaraDesenhada());
      anunciarEstado();
      diz(`par câmara ligada=${!!voz.camara}`);
    } catch (e) {
      diz(`par câmara NÃO ligou: ${e && e.message ? e.message : e}`);
    }

    // O anfitrião parte o ecrã; o convidado vai assistir. É o caminho que dá sentido ao
    // projeto e o único que nunca tinha sido visto entre dois pares.
    if (modo === '') {
      await esperar(3000);
      // Diretamente, sem o seletor: num teste automatico nao ha quem clique no menu.
      // O teste de par usa a qualidade REAL (a que sai do menu), para exercitar o mesmo
      // caminho do dono. As bandeiras escrevem-na antes, para se poder variar.
      localStorage.setItem('bruma.qualidade', JSON.stringify({
        modo: 'pers',
        altura: await invoke('autoteste_altura').catch(() => 1080),
        fps: await invoke('autoteste_fps').catch(() => 30),
        debito: 0,
        som: true,
      }));
      await iniciarPartilha(await invoke('autoteste_fonte').catch(() => 'ecra:1'));
      anunciarEstado();
      diz(`par ANFITRIAO a partilhar=${!!voz.ecra}`);
    }

    // Deixar correr, e depois contar. O que interessa é `recebidos`: prova que o datagrama
    // saiu de uma instância e chegou ao descodificador da outra.
    let conversa = null;
    for (let volta = 1; volta <= 6; volta++) {
      await esperar(5000);

      // A CONVERSA PRIVADA. Os dois lados abrem-na sem combinar nada -- o id sai das duas
      // chaves publicas e a chave sai do Diffie-Hellman entre elas. Se as derivacoes nao
      // forem simetricas, cada um escreve no seu log e nenhum ve o do outro: dois monologos
      // em vez de uma conversa, e sem um unico erro pelo caminho.
      if (volta === 3 && !conversa) {
        const outro = [...voz.presentes.keys()].find(k => k !== voz.eu);
        if (outro) {
          try {
            const id = await invoke('abrir_conversa', { peer: outro });
            const st = await invoke('estado');
            const c = st.conversas.find(x => x.id === id);
            conversa = c || { id, canal: 'conversa', nome: '?' };
            diz(`par conversa: id=${id} com=${outro.slice(0, 6)} nome="${conversa.nome}"`);
            await invoke('enviar', {
              servidor: id,
              canal: conversa.canal,
              texto: `privado de ${voz.eu.slice(0, 6)}`,
            });
          } catch (e) {
            diz(`par conversa FALHOU a abrir: ${e}`);
          }
        }
      }

      // E no fim: os dois lados tem de ver as DUAS mensagens, com nome e nao com
      // "desconhecido" -- se so virem a propria, a sincronizacao da conversa nao anda.
      if (volta === 6 && conversa) {
        const msgs = await invoke('mensagens', {
          servidor: conversa.id,
          canal: conversa.canal,
        }).catch(e => { diz(`par conversa FALHOU a ler: ${e}`); return []; });
        const st = await invoke('estado');
        diz(`par conversa mensagens: ${msgs.length}/2`
          + ` [${msgs.map(m => `${m.autor_nome}: ${m.texto}`).join(' | ')}]`
          + ` conversas-na-vista=${st.conversas.length}`
          + ` servidores-na-vista=${st.servidores.length}`);

        // O NAO-LIDO, de ponta a ponta e com duas maquinas.
        //
        // A logica da contagem ja tem teste proprio sem maquinas nenhumas. O que SO se
        // consegue ver aqui e o caminho todo: a mensagem do outro atravessa a rede, entra no
        // meu log, e o contador sobe -- e depois desce quando eu abro a conversa.
        const porLerAntes = (st.conversas.find(c => c.id === conversa.id) || {}).nao_lidos;

        // `marcar: false` SO LE. Aqui ha mesmo uma mensagem por ler, vinda da outra
        // instancia, portanto isto discrimina: sem o modo so-ler, a contagem caia a zero.
        await invoke('marcar_lido', {
          servidor: conversa.id, canal: conversa.canal, marcar: false,
        }).catch(() => -1);
        const stSoLer = await invoke('estado');
        const depoisDeSoLer = (stSoLer.conversas.find(c => c.id === conversa.id) || {}).nao_lidos;

        const antesDeMarcar = await invoke('marcar_lido', {
          servidor: conversa.id, canal: conversa.canal,
        }).catch(() => -1);
        const st2 = await invoke('estado');
        const porLerDepois = (st2.conversas.find(c => c.id === conversa.id) || {}).nao_lidos;
        // E as bolhas que a interface DESENHOU, e nao so os numeros: um contador certo com
        // uma bolha que ninguem pintou nao serve para nada.
        const bolhasAgora = document.querySelectorAll('.bolha').length;
        diz(`par nao lido: antes=${porLerAntes} so-ler-nao-mexeu=${depoisDeSoLer}`
          + ` depois-de-abrir=${porLerDepois}`
          + ` marca-devolveu-anterior=${antesDeMarcar >= 0} bolhas-no-ecra=${bolhasAgora}`);
      }

      const gente = [...voz.presentes.keys()];
      const estado = await invoke('qualidade', { peers: gente }).catch(() => []);
      const resumo = estado.map(e =>
        // Os RITMOS e não só os totais (#33): é isto que distingue «a chamada está viva»
        // de «a chamada esteve viva». E os recusados pelo transporte (#34), que até agora
        // não eram contados em lado nenhum — um par que recuse todos os datagramas dava
        // exactamente o mesmo que um par calado.
        `${e.peer.slice(0, 6)} ${e.relay ? 'relay' : 'direta'} ↑${e.enviados} ↓${e.recebidos}`
        + ` (${e.envS}/s ↑, ${e.recS}/s ↓)`
        + ` rtt=${typeof e.ms === 'number' && e.ms > 0
          ? (e.ms < 0.5 ? '<1ms' : Math.round(e.ms) + 'ms') : 'por-medir'}`
        // «datagramas-nao-saidos» e nao «recusados»: o guiao do teste de par procura a
        // palavra «recus» no registo para contar recusas do porteiro, e este texto fazia-o
        // acusar seis recusas que nunca existiram. Um medidor que dispara sobre si proprio
        // deixa de saber dizer quando ha mesmo alguma coisa.
        + ` datagramas-nao-saidos=${e.vozFalhados}`
        + ` perda=${typeof e.perda === 'number' ? e.perda.toFixed(1) + '%' : 'por medir'}`
        + ` ele-diz-ter-mandado=${e.disseTerEnviado}`
        + ` ultimo-rec=${typeof e.haQuantoRec === 'number' ? e.haQuantoRec + 'ms' : 'nunca'}`
        // O ESPACO LIVRE NA FILA (#173). E o unico numero que diz se os 16 KiB sao
        // apertados: se ele nunca desce perto de zero, a fila nunca esteve perto de encher.
        + ` fila-livre=${typeof e.filaLivre === 'number' ? e.filaLivre + 'B' : '?'}`
        // Os cortes tambem saem no --par (#65): sem isto, a unica forma de saber que a voz
        // picou era alguem estar a ouvi-la no momento.
        + (() => {
          const c = cortesDaVoz.get(e.peer);
          return c ? ` cortes=${c.total} folga=${Math.round(c.folga * 1000)}ms` : ' cortes=0';
        })()
      ).join(' | ');
      const ecra = estado.map(e => `ecrã ↑${e.ecraEnviado} ↓${e.ecraRecebido}`).join(' | ');
      // O que interessa na câmara é o mesmo que interessa no ecrã: não "chegaram bytes",
      // mas "saiu imagem". `frames` conta o que o descodificador DESENHOU.
      const cams = [...camarasRecebidas.entries()]
        .map(([k, c]) => `${k.slice(0, 6)} ${c.frames} frames`).join(' | ');
      // Um aviso sobre a partilha tem de CHEGAR a interface, e nao ficar num eprintln.
      if (partilhaAviso) diz(`par AVISO na interface: "${partilhaAviso.slice(0, 72)}"`);
      diz(`par ${volta}/6: ${gente.length} presente(s) ${resumo || '(sem ligações)'}`
        + ` | a ouvir ${voz.audio.size} | ${ecra || '—'}`
        + ` | câmaras: ${cams || 'nenhuma'} (anunciadas: ${voz.comCamara.size})`);

      // O anfitrião olha para o próprio ecrã A MEIO da transmissão — o cenário exato do
      // bug do ecrã preto: quem começa a ver tarde precisa do cabeçalho reenviado.
      if (modo === '' && volta === 2 && !voz.aVer) assistir(voz.eu);

      // Assim que alguém aparecer a transmitir, o convidado carrega em Assistir e conta o
      // que o <video> conseguiu mesmo descodificar — que é o que separa "chegaram bytes"
      // de "vê-se imagem".
      if (modo !== '' && !voz.aVer) {
        const quem = [...voz.aPartilhar][0];
        if (quem) {
          const t0 = performance.now();
          assistir(quem);
          // Volume a zero SO no teste: duas instancias na mesma maquina realimentam-se, e
          // o `--par` ja tocou som pelas colunas do dono uma vez. O `muted` fica como em
          // producao, que e o que se quer medir.
          setTimeout(() => {
            const e2 = ecraDe(quem);
            if (e2) e2.volume = 0;
          }, 300);
          diz(`par CONVIDADO a assistir a ${quem.slice(0, 6)}`);
          // Quanto tempo até APARECER imagem. É o número que o frame-chave fixo melhora:
          // sem ele, dependia da placa gráfica de quem partilha e não era determinável.
          (async () => {
            for (let i = 0; i < 300; i++) {
              const el = ecraDe(quem);
              const q = el && el.getVideoPlaybackQuality ? el.getVideoPlaybackQuality() : null;
              if (q && q.totalVideoFrames > 0) {
                return diz(`par PRIMEIRA IMAGEM em ${Math.round(performance.now() - t0)} ms`);
              }
              await esperar(100);
            }
            diz('par PRIMEIRA IMAGEM: nunca chegou em 30 s');
          })();
        }
      }
      if (voz.aVer) {
        const el = ecraDe(voz.aVer);
        const q = el && el.getVideoPlaybackQuality ? el.getVideoPlaybackQuality() : null;
        // O buffer distingue as duas avarias que dao o mesmo readyState=2: FOME (o buffer
        // acaba logo a seguir ao instante actual) e BURACO (ha dados a frente, mas com um
        // vazio pelo meio que o leitor nao salta). Sem isto, ficava-se a adivinhar.
        let faixas = '—';
        // Um buffer inteiro devia ser UMA faixa. Se são várias, faltam bocados pelo meio,
        // e é isso que se conta -- foi um buraco destes, com nome nenhum, que escondeu
        // durante versões que cada fragmento de ecrã ia pela rede DUAS vezes.
        let buracos = 0;
        let emFalta = 0;
        if (el && el.buffered) {
          for (let i = 1; i < el.buffered.length; i += 1) {
            const vazio = el.buffered.start(i) - el.buffered.end(i - 1);
            if (vazio > 0.02) { buracos += 1; emFalta += vazio; }
          }
          faixas = [];
          for (let i = 0; i < el.buffered.length; i++) {
            faixas.push(`${el.buffered.start(i).toFixed(2)}-${el.buffered.end(i).toFixed(2)}`);
          }
          faixas = faixas.join(' , ') || 'vazio';
        }
        diz(`par imagem: ${el ? `${el.videoWidth}x${el.videoHeight}` : 'sem <video>'}`
          + ` readyState=${el ? el.readyState : '-'}`
          + ` frames=${q ? q.totalVideoFrames : '?'}`
          + ` t=${el ? el.currentTime.toFixed(2) : '-'}`
          + ` buffer=[${faixas}] buracos=${buracos} (${emFalta.toFixed(2)}s)`
          + ` pedacos=${el ? el.__pedacos : '-'} fila-max=${el ? el.__filaMax : '-'} aparados=${el ? el.__aparados : '-'}`
          // O som da partilha ia dar a um elemento mudo e ninguem o ouvia. Aqui exige-se
          // que o elemento esteja destapado E que esteja mesmo a descodificar audio --
          // "tem faixa de audio" ja era verdade antes e nao provava nada.
          //
          // O volume vai a zero, e so no teste: duas instancias na mesma maquina fariam
          // realimentacao (o loopback de uma apanha o que a outra toca). O `muted`, esse,
          // fica como fica em producao -- e o que estava partido.
          + ` mudo=${el ? el.muted : '-'} audio-bytes=${el ? (el.webkitAudioDecodedByteCount || 0) : '-'}`
          + ` codec=${el && el.__codec ? el.__codec : '?'}`
          + ` erro=${el && el.error ? el.error.code : 'nenhum'}`);
      }
    }
  } catch (e) {
    diz(`par FALHOU: ${e}`);
  }
})();

/* ---------- a barra da janela é nossa -------------------------------------- */

/* A moldura do Windows saiu; estes botões e o arrasto tomam-lhe o lugar. O
   `-webkit-app-region: drag` do Chromium não funciona na WebView2 — o arrasto tem de ser
   pedido ao Tauri no mousedown, e é por isso que isto é JavaScript e não três linhas de
   CSS. */
(function barraDaJanela() {
  if (!window.__TAURI__) return;
  const janela = window.__TAURI__.window.getCurrentWindow();

  document.addEventListener('mousedown', ev => {
    if (ev.button !== 0) return;
    const barra = ev.target.closest('.topbar, .barra-instalador, .janela__botoes');
    if (!barra) return;
    // Um clique num botão, campo ou chip é para esse elemento, não para arrastar.
    if (ev.target.closest('button, input, textarea, select, a, .chip, [role="button"]')) return;
    janela.startDragging();
  });

  document.addEventListener('dblclick', ev => {
    const barra = ev.target.closest('.topbar, .barra-instalador');
    if (!barra || ev.target.closest('button, input, .chip, [role="button"]')) return;
    janela.toggleMaximize();
  });

  document.addEventListener('click', ev => {
    const bt = ev.target.closest('[data-janela]');
    if (!bt) return;
    ev.stopPropagation();
    const accao = bt.dataset.janela;
    if (accao === 'minimizar') janela.minimize();
    else if (accao === 'maximizar') janela.toggleMaximize();
    else if (accao === 'fechar') janela.close();
  });
})();

/* ---------- medir a interface, para se poder verificar sem olhos ----------- */

(async () => {
  if (!window.__TAURI__) return;
  if (!(await invoke('medir_ui_pedido').catch(() => false))) return;
  const diz = linha => invoke('capacidades', { linha }).catch(() => {});
  await new Promise(r => setTimeout(r, 1200));

  const medir = sel => {
    const el = document.querySelector(sel);
    if (!el) return `${sel}: NAO EXISTE`;
    const r = el.getBoundingClientRect();
    const est = getComputedStyle(el);
    return `${sel}: ${Math.round(r.left)},${Math.round(r.top)} ${Math.round(r.width)}x${Math.round(r.height)}`
      + ` vis=${est.visibility} disp=${est.display} op=${est.opacity} z=${est.zIndex}`;
  };
  diz(`ui janela=${innerWidth}x${innerHeight}`);
  for (const s of ['.janela__botoes', '.janela__bt', '.janela__bt--fechar', '.topbar', '.transport', '.members']) {
    diz('ui ' + medir(s));
  }
  const bt = document.querySelector('.janela__bt--fechar');
  if (bt) {
    const r = bt.getBoundingClientRect();
    const emCima = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
    diz(`ui quem esta no ponto do fechar: ${emCima ? emCima.className || emCima.tagName : 'nada'}`);
  }

  // E o seletor de fontes, que só existe aberto: abre-se, espera-se pelas miniaturas,
  // mede-se a grelha e fecha-se. Foi um erro de cascata aqui (caixa a 420px, uma coluna)
  // que só um screenshot do dono apanhou — nunca mais sem medição.
  try {
    await escolherFonte();
  } catch (e) {
    diz(`ui seletor REBENTOU ao abrir: ${e && e.message ? e.message : e}`);
  }
  await new Promise(r => setTimeout(r, 3500));
  const caixaF = document.querySelector('.caixa--fontes');
  const grelha = document.querySelector('.fontes');
  const cartoes = document.querySelectorAll('.fonte');
  if (caixaF && grelha) {
    const colunas = getComputedStyle(grelha).gridTemplateColumns.split(' ').length;
    const c1 = cartoes[0] ? cartoes[0].getBoundingClientRect() : null;
    diz(`ui seletor: caixa=${Math.round(caixaF.getBoundingClientRect().width)}px`
      + ` colunas=${colunas} cartoes=${cartoes.length}`
      + (c1 ? ` primeiro=${Math.round(c1.width)}x${Math.round(c1.height)}` : ''));
    // E os dois separadores, um a um: cada um só pode mostrar o seu tipo.
    for (const aba of document.querySelectorAll('#abas-fontes .aba')) {
      aba.click();
      const n = document.querySelectorAll('.fonte').length;
      diz(`ui aba ${aba.dataset.aba}: ${n} cartoes`);
    }
    // A engrenagem abre o MENU do modo de transmissão; percorre-se tudo.
    // Estado conhecido primeiro: uma pessoa a mexer na janela durante a medição (já
    // aconteceu) abre e fecha o menu debaixo dos números. Mede-se do fechado.
    const menu = $('#menu-transmissao');
    menu.hidden = true;
    $('#btn-qualidade').click();
    const r = menu.getBoundingClientRect();
    diz(`ui menu: aberto=${!menu.hidden} ${Math.round(r.width)}x${Math.round(r.height)}`
      + ` modos=${document.querySelectorAll('[data-modo]').length}`
      + ` resumo="${$('#resumo-qualidade').textContent}"`);
    for (const nome of ['altura', 'fps', 'debito']) {
      const sub = $('#sub-' + nome);
      document.querySelector(`[data-abre="${nome}"]`).click();
      const aberto = !sub.hidden && sub.getBoundingClientRect().height > 0;
      document.querySelector(`[data-abre="${nome}"]`).click();
      // O atributo hidden não chega: o bug dos submenus eternos era o display:flex a
      // vencê-lo. Mede-se a ALTURA REAL, que é o que os olhos veem.
      const fechado = sub.getBoundingClientRect().height === 0;
      diz(`ui sub ${nome}: ${document.querySelectorAll('#sub-' + nome + ' .menu-trans__opcao').length} opções`
        + ` abre=${aberto} fecha=${fechado}`);
    }
    // ---- o palco da transmissão ------------------------------------------------
    //
    // Mede-se com estado FABRICADO: finge-se uma sala com duas pessoas, uma a transmitir,
    // e conta-se o que a vista produz. É a única forma de exercitar aqui um palco que, na
    // vida real, precisa de duas máquinas ligadas.
    {
      const antes = {
        eu: voz.eu, canal: voz.canal, servidor: voz.servidor, ecra: voz.ecra,
        aVer: voz.aVer, tam: voz.ecraTamanho, srv: servidorAtual, cnl: canalAtual,
        qual: voz.qualidadeEmUso,
      };
      // Uma sala de voz de verdade, criada para a medição — a vista olha para o canal
      // SELECIONADO, e sem um canal de voz escolhido ela nem chega a desenhar.
      let srv = vista.servidores.find(x => x.canais.some(c => c.tipo === 'voz'));
      if (!srv) {
        const id = await invoke('criar_servidor', { nome: 'medicao' });
        await invoke('criar_canal', { servidor: id, nome: 'palco', tipo: 'voz' });
        await desenharTudo();
        srv = vista.servidores.find(x => x.id === id);
      }
      const cv = srv.canais.find(c => c.tipo === 'voz');
      servidorAtual = srv.id; canalAtual = cv.id;
      voz.eu = 'eueueu';
      voz.canal = cv.id;
      voz.servidor = srv.id;
      voz.presentes.set('outro1', voz.canal);
      voz.ecra = { fechar() {} };
      voz.ecraTamanho = { largura: 2560, altura: 1440 };
      voz.aSerVistoPor.add('outro1');
      voz.aPartilhar.add('outro1');
      voz.infoDaTransmissao.set('outro1', { qualidade: '1080p 30FPS', espectadores: 3 });
      voz.aVer = voz.eu;
      // Os três casos do rótulo, com estado fabricado: a fonte mais pequena que o pedido
      // (o caso do dono), do tamanho certo, e "Nativa".
      for (const [pedida, real, nome] of [[1440, 1050, 'fonte menor'],
                                          [1440, 1440, 'igual'],
                                          [0, 1200, 'nativa']]) {
        voz.qualidadeEmUso = { altura: pedida, fps: 60, debito: 0, som: true };
        voz.ecraTamanho = { largura: Math.round(real * 16 / 9), altura: real };
        diz(`ui rotulo (${nome}): "${rotuloDaQualidade()}" | ${porqueEstaResolucao()}`);
      }
      voz.qualidadeEmUso = { altura: 1440, fps: 60, debito: 0, som: true };
      gentePorBaixoOculta = false;
      janelaComFoco = true;
      desenharVoz();

      const palco = document.querySelector('.palco');
      const selos = [...document.querySelectorAll('.palco__selo')].map(e => e.textContent.trim());
      const bts = document.querySelectorAll('.palco__meio .palco__bt');
      diz('ui palco: existe=' + !!palco
        + ' selos=' + JSON.stringify(selos)
        + ' botoes=' + bts.length
        + ' onde="' + ($('.palco__onde') ? $('.palco__onde').textContent.trim() : '') + '"'
        + ' convidar=' + !!document.querySelector('.palco__baixo-esq .palco__bt')
        + ' fotinhas=' + document.querySelectorAll('.mini').length
        + ' aoVivoNasFotinhas=' + document.querySelectorAll('.mini__vivo').length
        + ' tampasDeAssistir=' + document.querySelectorAll('.mini__tampa').length);

      // O botão do meio esconde as fotinhas, e volta a mostrá-las.
      const antesOculto = document.querySelector('.palco__gente').hidden;
      [...bts].find(b => /Ocultar/.test(b.title)).click();
      const depoisOculto = document.querySelector('.palco__gente').hidden;
      [...document.querySelectorAll('.palco__meio .palco__bt')]
        .find(b => /Mostrar/.test(b.title)).click();
      const reposto = document.querySelector('.palco__gente').hidden;
      diz('ui palco ocultar: antes=' + antesOculto + ' depois=' + depoisOculto
        + ' reposto=' + reposto);

      // Sem foco, aparece a mensagem de pausa — e SÓ para quem transmite.
      janelaComFoco = false;
      desenharVoz();
      const pausa = document.querySelector('.palco__pausa');
      diz('ui palco pausa: aparece=' + !!pausa
        + ' texto="' + (pausa ? pausa.textContent.replace(/\s+/g, ' ').trim().slice(0, 46) : '') + '"');
      janelaComFoco = true;

      // E o palco de OUTRA pessoa: sem botão de parar transmissão, com o nome dela.
      voz.aVer = 'outro1';
      desenharVoz();
      const bts2 = [...document.querySelectorAll('.palco__meio .palco__bt')].map(b => b.title);
      diz('ui palco alheio: botoes=' + JSON.stringify(bts2.map(t => t.split(' ')[0]))
        + ' qualidade="' + ([...document.querySelectorAll('.palco__selo')][0] || {}).textContent + '"'
        + ' semPausa=' + !document.querySelector('.palco__pausa'));

      voz.presentes.delete('outro1');
      voz.aSerVistoPor.delete('outro1');
      voz.aPartilhar.delete('outro1');
      voz.infoDaTransmissao.delete('outro1');
      Object.assign(voz, { eu: antes.eu, canal: antes.canal, servidor: antes.servidor,
        ecra: antes.ecra, aVer: antes.aVer, ecraTamanho: antes.tam,
        qualidadeEmUso: antes.qual });
      servidorAtual = antes.srv; canalAtual = antes.cnl;
      desenharVoz();
    }

    // ---- as avarias que antes eram invisiveis ---------------------------------
    {
      const antes = { eu: voz.eu, canal: voz.canal, srv: servidorAtual, cnl: canalAtual };
      let srv = vista.servidores.find(x => x.canais.some(c => c.tipo === 'voz'));
      if (!srv) {
        const id = await invoke('criar_servidor', { nome: 'medicao' });
        await invoke('criar_canal', { servidor: id, nome: 'palco', tipo: 'voz' });
        await desenharTudo();
        srv = vista.servidores.find(x => x.id === id);
      }
      const cv = srv.canais.find(c => c.tipo === 'voz');
      servidorAtual = srv.id; canalAtual = cv.id;
      voz.eu = 'eueueu'; voz.canal = cv.id; voz.servidor = srv.id;
      voz.presentes.set('outro1', cv.id);

      // 1) a minha voz morreu: o botao do microfone tem de dizer PORQUE.
      vozFalhou = 'O codificador de voz desistiu — ninguém te ouve.';
      desenharRodape(); await new Promise(r => setTimeout(r, 250));
      diz(`ui voz morta: cortado=${$('#btn-mic').classList.contains('is-cortado')}`
        + ` title-diz-porque=${/desistiu/.test($('#btn-mic').title)}`);
      vozFalhou = null;

      // 2) deixei de ouvir UMA pessoa: ela tem de ficar marcada.
      vozPartida.set('outro1', 'o áudio desta pessoa não está a descodificar');
      voz.aVer = null; desenharVoz();
      const marcas = document.querySelectorAll('.tile__sem-audio');
      diz(`ui voz de um so: marcas=${marcas.length}`
        + ` no-painel-certo=${!!document.querySelector('.tile[data-chave="outro1"] .tile__sem-audio')}`);
      vozPartida.clear();

      // 3) o codec que esta maquina nao le: em vez de preto, a razao.
      const falso = { porqueNaoDa: () => 'Esta máquina não sabe descodificar avc1.640033.' };
      fluxosRecebidos.set('outro1', falso);
      voz.aPartilhar.add('outro1'); voz.aVer = 'outro1';
      desenharVoz();
      const pausa = document.querySelector('.palco__pausa');
      diz(`ui codec recusado: explica=${!!pausa}`
        + ` texto="${pausa ? pausa.textContent.replace(/\s+/g, ' ').slice(0, 46) : ''}"`);

      fluxosRecebidos.delete('outro1'); voz.aPartilhar.delete('outro1');
      voz.presentes.delete('outro1'); voz.aVer = null;
      Object.assign(voz, { eu: antes.eu, canal: antes.canal });
      servidorAtual = antes.srv; canalAtual = antes.cnl;
      desenharVoz();
    }

    // ---- o modo privado -------------------------------------------------------
    //
    // O que se mede aqui e sobretudo uma coisa: que ficar no modo privado AGUENTA. O
    // `desenharTudo` tinha uma auto-seleccao que salta para o primeiro servidor quando o
    // actual nao existe -- e no modo privado o actual e nulo de proposito. Cada mensagem
    // que chega dispara um `servidor-mudou`, portanto isso atirava a pessoa de volta para
    // um servidor de segundo a segundo, sem nada a explicar porque.
    {
      const antes = { modo, servidor: servidorAtual };
      irParaPrivado();
      await new Promise(r => setTimeout(r, 200));
      const railActivo = $('#btn-privado').classList.contains('is-active');
      const membrosEscondidos = $('#bloco-membros').hidden;
      const semConvite = $('#btn-convite').style.display === 'none';
      const arroba = $('#glifo-canal').textContent;
      const listaTem = $('#lista-canais').textContent.trim().length;

      // E agora o que acontece a cada mensagem que chega. O que muda nao e o `modo` --
      // nada lhe toca -- e sim o `servidorAtual`, que a auto-seleccao repunha no primeiro
      // servidor por baixo dos panos. Nao se via nada no imediato, e depois voltar a um
      // servidor levava a pessoa para o primeiro em vez de para onde ela estava.
      //
      // Mede-se ISSO, e nao o `modo`: a primeira versao desta medicao passava com e sem a
      // correccao, ou seja, nao media nada.
      servidorAtual = null;
      await desenharTudo();
      await new Promise(r => setTimeout(r, 150));
      await desenharTudo();
      const aguentou = modo === 'privado' && servidorAtual === null;

      diz(`ui privado: modo=${modo} rail-activo=${railActivo}`
        + ` membros-escondidos=${membrosEscondidos} sem-convite=${semConvite}`
        + ` glifo="${arroba}" lista=${listaTem > 0} conversas=${(vista.conversas || []).length}`
        + ` nao-mexeu-no-servidor=${aguentou}`);

      // A caixa de escrita tem de saber para onde escreve -- ou dizer que nao sabe.
      const destinoPrivado = destinoDeEscrita();
      escolherServidor(antes.servidor);
      await new Promise(r => setTimeout(r, 150));
      const destinoServidor = destinoDeEscrita();
      diz(`ui privado destino: sem-conversa=${JSON.stringify(destinoPrivado)}`
        + ` no-servidor=${destinoServidor ? 'canal ' + String(destinoServidor.canal).slice(0, 6) : 'nenhum'}`
        + ` voltou-a-servidor=${modo === 'servidor'}`);
    }

    // ---- as permissoes ---------------------------------------------------------
    //
    // O que se mede e o EFEITO, e nao o botao. Uma definicao de privacidade que so muda um
    // valor guardado e pior do que nao existir: promete uma coisa e nao a faz.
    {
      const alguem = 'bb'.repeat(32);
      const inicio = await invoke('permissoes');

      // Bloquear tem de aparecer na lista E tirar a pessoa dos amigos, senao a app fica a
      // dizer duas coisas contrarias sobre a mesma pessoa.
      await invoke('adicionar_amigo', { chave: alguem, nome: 'a bloquear' }).catch(() => {});
      const eraAmigo = (await invoke('amigos')).some(x => x.chave === alguem);
      await invoke('bloquear', { chave: alguem, sim: true }).catch(() => {});
      const dep = await invoke('permissoes');
      const ficouAmigo = (await invoke('amigos')).some(x => x.chave === alguem);

      // A politica tem de ficar guardada, e so aceitar o que conhece.
      await invoke('definir_quem_escreve', { politica: 'salas' }).catch(() => {});
      const politica = (await invoke('permissoes')).quem_escreve;
      let recusouLixo = false;
      try {
        await invoke('definir_quem_escreve', { politica: 'seja-o-que-for' });
      } catch (e) { recusouLixo = true; }

      // Desbloquear devolve ao princípio.
      await invoke('bloquear', { chave: alguem, sim: false }).catch(() => {});
      const limpo = !(await invoke('permissoes')).bloqueados.includes(alguem);

      let recusouChaveMa = false;
      try {
        await invoke('bloquear', { chave: 'nao-e-chave', sim: true });
      } catch (e) { recusouChaveMa = true; }

      await invoke('definir_quem_escreve', { politica: inicio.quem_escreve }).catch(() => {});
      await invoke('remover_amigo', { chave: alguem }).catch(() => {});

      diz(`ui permissoes: bloqueou=${dep.bloqueados.includes(alguem)}`
        + ` era-amigo=${eraAmigo} deixou-de-ser=${!ficouAmigo}`
        + ` politica-guardada=${politica === 'salas'} recusou-politica-invalida=${recusouLixo}`
        + ` desbloqueou=${limpo} recusou-chave-invalida=${recusouChaveMa}`);
    }

    // ---- os amigos ------------------------------------------------------------
    //
    // Uma lista de amigos e uma DECISAO minha guardada na minha maquina, e o que se mede e
    // que cada controlo faz mesmo o que diz -- nao que o botao existe.
    {
      const falso = 'aa'.repeat(32);
      const antes = (await invoke('amigos')).length;
      await invoke('adicionar_amigo', { chave: falso, nome: 'Alguem' }).catch(() => {});
      const depois = await invoke('amigos');
      const posto = depois.find(x => x.chave === falso);

      // Adicionar duas vezes RENOMEIA, nao duplica -- senao a lista enche-se de repetidos.
      await invoke('adicionar_amigo', { chave: falso, nome: 'Outro nome' }).catch(() => {});
      const depois2 = await invoke('amigos');
      const repetidos = depois2.filter(x => x.chave === falso).length;
      const renomeou = (depois2.find(x => x.chave === falso) || {}).nome === 'Outro nome';

      // A verificacao da chave tem de ficar guardada.
      await invoke('marcar_verificado', { chave: falso, verificado: true }).catch(() => {});
      const marcado = ((await invoke('amigos')).find(x => x.chave === falso) || {}).verificado;

      // E o que tem de ser recusado.
      const recusas = [];
      for (const [c, n, porque] of [
        [vista.chave, 'eu', 'a minha propria chave'],
        ['nao-e-uma-chave', 'x', 'lixo'],
        [falso, '   ', 'sem nome'],
      ]) {
        try {
          await invoke('adicionar_amigo', { chave: c, nome: n });
          recusas.push(`ACEITOU ${porque}`);
        } catch (e) { /* recusou, como deve */ }
      }

      await invoke('remover_amigo', { chave: falso }).catch(() => {});
      const sobrou = (await invoke('amigos')).some(x => x.chave === falso);

      diz(`ui amigos: antes=${antes} pos=${!!posto} repetidos=${repetidos}`
        + ` renomeou=${renomeou} verificado-guardado=${marcado === true}`
        + ` removeu=${!sobrou} recusas-em-falta=${JSON.stringify(recusas)}`);
    }

    // ---- a decisao de que conversa mostrar -------------------------------------
    //
    // Isolada de proposito: a versao anterior deste teste precisava de DUAS maquinas ligadas
    // e de uma conversa a existir, e por isso ora corria ora nao. Uma medicao que so as
    // vezes corre nao distingue "passou" de "nao foi tentado".
    {
      const cs = [{ id: 'aaa' }, { id: 'bbb' }];
      const casos = [
        ['Amigos aguenta com conversas a existir', qualConversa(null, cs), null],
        ['a escolhida mantem-se', qualConversa('bbb', cs), 'bbb'],
        ['uma que desapareceu cai na primeira', qualConversa('zzz', cs), 'aaa'],
        ['sem conversas nenhumas fica nos Amigos', qualConversa('zzz', []), null],
      ];
      const falhas = casos.filter(([, deu, devia]) => deu !== devia)
        .map(([o, deu, devia]) => `${o}: deu ${deu}, devia ${devia}`);
      diz(`ui conversa escolhida: ${casos.length - falhas.length}/${casos.length}`
        + ` falhas=${JSON.stringify(falhas)}`);
    }

    // ---- o que a amizade SERVE ------------------------------------------------
    //
    // Falar com alguem com quem NAO se partilha servidor nenhum. Se isto nao funcionar, uma
    // lista de amigos e so uma lista: bonita e inutil.
    //
    // So corre quando ha exactamente um amigo posto de fora (BRUMA_AMIGO), para nao mexer na
    // lista de quem esta a usar a app.
    {
      const lista = await invoke('amigos').catch(() => []);
      const teste = lista.filter(a => a.nome === 'amigo de teste');
      if (teste.length === 1) {
        const dele = teste[0].chave;
        let id = null;
        let erro = '';
        // O `abrir_conversa` so consegue com a chave de conversa dele -- e essa so chega
        // pelo `Ola`, ou seja, so se nos tivermos MESMO ligado. E por isso a prova.
        for (let i = 0; i < 40 && !id; i++) {
          await new Promise(r => setTimeout(r, 500));
          id = await invoke('abrir_conversa', { peer: dele }).catch(e => { erro = String(e); return null; });
        }
        let msgs = [];
        if (id) {
          await invoke('enviar', { servidor: id, canal: 'conversa', texto: 'ola sem servidor' })
            .catch(e => { erro = String(e); });
          for (let i = 0; i < 20 && msgs.length < 2; i++) {
            await new Promise(r => setTimeout(r, 500));
            msgs = await invoke('mensagens', { servidor: id, canal: 'conversa' }).catch(() => []);
          }
        }
        const st = await invoke('estado');
        // COM uma conversa a existir, a vista dos Amigos tem de continuar alcancavel -- era
        // aqui que a auto-seleccao a roubava, e com ela o unico botao de Remover.
        let porque = 'nao corri';
        if (id) {
          // O MODO tem de ser privado, senao isto mede o caminho dos servidores e diz que
          // nao, por uma razao que nada tem a ver com o que se afirma. Ja me enganou tres
          // vezes hoje -- por isso a medicao passa a dizer PORQUE falhou.
          irParaPrivado();
          await new Promise(r => setTimeout(r, 120));
          conversaAtual = null;
          await desenharTudo();
          await new Promise(r => setTimeout(r, 120));
          await desenharTudo();
          const texto = $('#stream').textContent;
          porque = modo !== 'privado' ? `modo=${modo}`
            : conversaAtual !== null ? 'a auto-seleccao roubou a vista'
              : !texto.includes('Amigos') ? 'a vista nao desenhou'
                : !/Remover|Adicionar/.test(texto) ? 'sem os botoes de gerir'
                  : 'sim';
        }

        diz(`ui amigos alcancavel com conversa: ${porque}`);
        diz(`ui amigo sem servidor: ligou=${!!id} conversa=${id ? id.slice(0, 12) : '-'}`
          + ` mensagens=${msgs.length} servidores=${st.servidores.length}`
          + ` conversas=${st.conversas.length} erro="${erro.slice(0, 60)}"`);
      }
    }

    // ---- as Definicoes, todas as seccoes --------------------------------------
    //
    // Nao se mede so "abriu": desenha-se CADA seccao e exige-se que cada uma produza um
    // titulo e conteudo. Uma seccao que rebenta a desenhar deixaria um painel em branco, e
    // um painel em branco parece uma definicao que ainda nao foi feita -- exactamente o
    // que este painel tem de saber distinguir.
    {
      await abrirDefinicoes();
      const lado = $('#defs');
      const itens = [...document.querySelectorAll('.defs__item')].map(b => b.textContent.trim());
      const grupos = [...document.querySelectorAll('.defs__grupo')].map(b => b.textContent.trim());
      diz(`ui defs: aberto=${!lado.hidden} seccoes=${itens.length} grupos=${JSON.stringify(grupos)}`
        + ` avatar=${!!$('#defs-avatar').style.backgroundImage}`
        + ` busca=${!!$('#defs-buscar')}`);

      // "Editar perfil" tem de LEVAR a algum lado, e o nome tem de mudar mesmo. Verificar
      // que o botão existe não distingue um atalho de um enfeite.
      await mostrarPainel('sistema');
      $('#defs-editar').click();
      await new Promise(r => setTimeout(r, 120));
      const foiParaConta = painelActivo === 'conta' && !!$('#def-nome');
      const antigo = vista.nome || '';
      $('#def-nome').value = 'medicao-' + antigo;
      [...document.querySelectorAll('#defs-painel .btn--primary')]
        .find(b => b.textContent === 'Guardar').click();
      await new Promise(r => setTimeout(r, 500));
      const mudou = vista.nome === 'medicao-' + antigo;
      $('#def-nome').value = antigo;
      [...document.querySelectorAll('#defs-painel .btn--primary')]
        .find(b => b.textContent === 'Guardar').click();
      await new Promise(r => setTimeout(r, 500));
      diz(`ui defs perfil: editar-leva-a-conta=${foiParaConta} nome-mudou=${mudou}`
        + ` reposto=${vista.nome === antigo} sidebar="${$('#defs-nome').textContent}"`);

      let falharam = [];
      let vazias = 0;
      for (const chave of ORDEM) {
        try {
          await mostrarPainel(chave);
          const painel = $('#defs-painel');
          const titulo = painel.querySelector('h2');
          const conteudo = painel.textContent.trim().length;
          if (!titulo || conteudo < 60) falharam.push(chave);
          if (PAINEIS[chave].vazia) {
            // As vazias TEM de dizer que estao vazias -- e nao mostrar um painel mudo.
            const diz_o = /ainda não existe|não vai existir/i.test(painel.textContent);
            if (!diz_o) falharam.push(chave + ':nao-avisa');
            vazias += 1;
          }
        } catch (e) {
          falharam.push(`${chave}:${e && e.message ? e.message : e}`);
        }
      }
      diz(`ui defs paineis: ${ORDEM.length} desenhados, ${vazias} honestamente vazios,`
        + ` falharam=${JSON.stringify(falharam)}`);

      // A busca filtra, e o que nao existe diz-se.
      $('#defs-buscar').value = 'voz';
      desenharMenuDeDefinicoes('voz');
      const comVoz = document.querySelectorAll('.defs__item').length;
      $('#defs-buscar').value = 'xpto';
      desenharMenuDeDefinicoes('xpto');
      const semNada = !!document.querySelector('.defs__nada');
      $('#defs-buscar').value = '';
      desenharMenuDeDefinicoes('');
      diz(`ui defs busca: "voz"=${comVoz} item(ns), "xpto" diz-que-nao-ha=${semNada},`
        + ` limpa=${document.querySelectorAll('.defs__item').length}`);

      // As 24 palavras vivem na Conta, que e onde alguem as iria procurar.
      await mostrarPainel('conta');
      const ver = [...document.querySelectorAll('#defs-painel .btn')]
        .find(b => /Mostrar as palavras/.test(b.textContent));
      ver.click();
      await new Promise(r => setTimeout(r, 400));
      const ps = [...document.querySelectorAll('#defs-painel .palavras span')]
        .map(l => l.textContent.replace(/^\s*\d+\s*/, ''));
      diz(`ui defs palavras: ${ps.length} mostradas, distintas=${new Set(ps).size},`
        + ` vazias=${ps.filter(x => !x.trim()).length}, botao-escondido=${ver.hidden}`);

      // Uma palavra trocada NAO pode restaurar. Verifica-se o resultado, nao a redaccao.
      [...document.querySelectorAll('#defs-painel .btn')]
        .find(b => /Restaurar de outras/.test(b.textContent)).click();
      $('#palavras-entrada').value = ps.slice(0, 23).join(' ') + ' zebra';
      [...document.querySelectorAll('#defs-painel .btn--perigo')][0].click();
      await new Promise(r => setTimeout(r, 500));
      const msg = $('#restaurar-nota').textContent;
      diz(`ui defs restauro: recusado=${!/restaurada/i.test(msg) && msg.length > 0}`
        + ` msg="${msg.slice(0, 46)}"`);
      $('#palavras-entrada').value = '';

      // Sair da chamada tem de acabar com os pedidos de assistir. Sobreviviam, e entao
      // voltar a entrar no mesmo canal trazia espectadores fantasma -- o olho dizia "1"
      // com ninguem a ver, e a lista enviada ao Rust voltava a incluir quem nao pediu
      // nada, com uma copia inteira de upload a sair para ele.
      voz.aSerVistoPor.add('espectador-de-mentira');
      const antesDeSair = voz.aSerVistoPor.size;
      await sairDeVoz(false);
      diz(`ui espectadores: antes=${antesDeSair} depois-de-sair=${voz.aSerVistoPor.size}`);

      // O interruptor da privacidade tem de CALAR a pergunta, não esconder a resposta.
      await mostrarPainel('dados');
      const alvo = [...document.querySelectorAll('#defs-painel label.def__linha')]
        .find(l => /não olhar/i.test(l.textContent))
        .querySelector('input[type=checkbox]');
      const ligado = () => localStorage.getItem(SEM_JOGO) === '1';
      if (ligado()) alvo.click();                  // garantir que começa ligada
      await new Promise(r => setTimeout(r, 60));
      const antes = perguntasSobreJanelas;
      await verJogo();
      const comDeteccao = perguntasSobreJanelas - antes;
      alvo.click();                                 // desligar a deteção
      await new Promise(r => setTimeout(r, 60));
      const meio = perguntasSobreJanelas;
      await verJogo();
      const semDeteccao = perguntasSobreJanelas - meio;
      // Comparar `horaCurta` com `horaCurta` não prova nada. O que se afirma é que o
      // formato deixou de estar escrito à mão -- por isso pergunta-se ao sistema qual ele
      // DIZ ser, e exige-se que a hora escrita corresponda.
      const quinzeE42 = Date.UTC(2026, 0, 2, 15, 42);
      const nossa = horaCurta(quinzeE42);
      const opc = new Intl.DateTimeFormat([], { hour: '2-digit' }).resolvedOptions();
      const temSufixo = /[ap]\.?\s?m\.?/i.test(nossa);
      const antiga = (d => `${String(d.getHours()).padStart(2, '0')}`
        + `:${String(d.getMinutes()).padStart(2, '0')}`)(new Date(quinzeE42));
      diz(`ui defs hora: "${nossa}" sistema-diz-12h=${!!opc.hour12} escreve-sufixo=${temSufixo}`
        + ` coerente=${!!opc.hour12 === temSufixo} fuso-aplicado=${!/15:42/.test(nossa)}`
        + ` difere-do-antigo=${nossa !== antiga}`);
      // Esta máquina está em 24 h, por isso o ramo dos 12 h nunca correria aqui — e um ramo
      // que nunca corre é um ramo por verificar. Força-se um locale de 12 h para provar que
      // o formato responde mesmo ao sistema, em vez de calhar coincidir com o antigo.
      const eua = new Date(quinzeE42).toLocaleTimeString('en-US', { hour: '2-digit', minute: '2-digit' });
      const pt = new Date(quinzeE42).toLocaleTimeString('pt-PT', { hour: '2-digit', minute: '2-digit' });
      diz(`ui defs hora 12h: en-US="${eua}" tem-sufixo=${/[AP]M/.test(eua)}`
        + ` pt-PT="${pt}" sao-diferentes=${eua !== pt}`);

      // O enquadramento. O que se afirma agora nao e "os itens estao ao meio" -- e que a
      // CAIXA nao ocupa o ecra: tem margem dos dois lados, a app ve-se por tras, e o
      // conjunto esta centrado. Foi a diferenca entre a primeira correccao e a segunda, e
      // o dono teve de a apontar duas vezes porque eu media a coisa errada.
      await mostrarPainel('conta');
      const medeCaixa = (etiqueta, larguraFingida) => {
        const veu = $('#defs');
        if (larguraFingida) veu.style.width = larguraFingida + 'px';
        const cx = $('#defs-caixa').getBoundingClientRect();
        const larg = larguraFingida || document.documentElement.clientWidth;
        const esq = Math.round(cx.left);
        const dir = Math.round(larg - cx.right);
        veu.style.width = '';
        return `${etiqueta}: janela=${larg} caixa=${Math.round(cx.width)}x${Math.round(cx.height)}`
          + ` margem-esq=${esq} margem-dir=${dir}`
          + ` centrada=${Math.abs(esq - dir) <= 2} sobra-app-a-vista=${esq > 0 && dir > 0}`;
      };
      diz('ui defs caixa ' + medeCaixa('real', 0));
      await new Promise(r => setTimeout(r, 60));
      $('#defs').style.width = '2200px';
      await new Promise(r => setTimeout(r, 60));
      {
        const cx = $('#defs-caixa').getBoundingClientRect();
        const esq = Math.round(cx.left);
        const dir = Math.round(2200 - cx.right);
        diz(`ui defs caixa largo: fingindo 2200 caixa=${Math.round(cx.width)}`
          + ` margem-esq=${esq} margem-dir=${dir} centrada=${Math.abs(esq - dir) <= 2}`
          + ` nao-ocupa-o-ecra=${cx.width < 2200 - 200}`);
      }
      $('#defs').style.width = '760px';
      await new Promise(r => setTimeout(r, 60));
      {
        const cx = $('#defs-caixa').getBoundingClientRect();
        const pn = $('#defs-painel').getBoundingClientRect();
        diz(`ui defs caixa estreito: fingindo 760 caixa=${Math.round(cx.width)}`
          + ` painel=${Math.round(pn.width)} cabe=${cx.right <= 760 && pn.width > 200}`);
      }
      $('#defs').style.width = '';
      await new Promise(r => setTimeout(r, 60));

      const rs = ['ha', 'nao', 'falhou', undefined].map(respostaDaProcura);
      diz(`ui defs update: distintas=${new Set(rs.slice(0, 3)).size}/3`
        + ` falhou-nao-mente=${rs[2] !== rs[1] && /não consegui/.test(rs[2])}`
        + ` desconhecido-cai-no-seguro=${rs[3] === rs[2]}`);

      diz(`ui defs janelas: guardada=${ligado()} perguntas com=${comDeteccao} sem=${semDeteccao}`
        + ` cartao-escondido=${$('#jogo').hidden} jogo-esquecido=${jogoAberto === null}`);
      if (ligado()) alvo.click();                   // deixar como estava
      fecharDefinicoes();
    }


    const rotuloAudio = $('#linha-som').querySelector('b').getBoundingClientRect();
    const antes = $('#mudo-transmissao').checked;
    $('#linha-som').click();
    const depois = $('#mudo-transmissao').checked;
    $('#linha-som').click();
    diz(`ui silenciar: rotulo ${Math.round(rotuloAudio.width)}x${Math.round(rotuloAudio.height)}`
      + ` (uma linha se altura < 22) alterna=${antes !== depois}`
      + ` reposto=${$('#mudo-transmissao').checked === antes}`);
    document.querySelector('[data-modo="jogos"]').click();
    diz(`ui modo jogos: resumo="${$('#resumo-qualidade').textContent}"`);

    // ---- a versao do outro lado deixa de ser muda ------------------------------
    //
    // Mede-se a DECISAO: quando e que a etiqueta aparece. O efeito completo precisa de duas
    // maquinas com versoes diferentes, o que nenhum teste local alcanca -- mas a regra ("so
    // quando difere, e um par sem campo conta como anterior") e verificavel aqui.
    {
      const guardadaV = minhaVersao;
      const alvoV = 'dd'.repeat(32);
      minhaVersao = '0.18.0';

      versaoDoPar.delete(alvoV);
      const semDizer = avisoDeVersao(alvoV);          // ainda nao disse nada
      versaoDoPar.set(alvoV, '0.18.0');
      const igual = avisoDeVersao(alvoV);              // mesma versao: nao ha que dizer
      versaoDoPar.set(alvoV, '0.19.0');
      const maisNova = avisoDeVersao(alvoV);           // ele a frente
      versaoDoPar.set(alvoV, 'anterior a 0.18');
      const antiga = avisoDeVersao(alvoV);             // ele atras, sem campo

      versaoDoPar.delete(alvoV);
      minhaVersao = guardadaV;

      diz(`ui versao do par: calado-sem-saber=${semDizer === null}`
        + ` calado-se-igual=${igual === null}`
        + ` avisa-se-mais-nova=${!!maisNova && maisNova.includes('0.19.0')}`
        + ` avisa-se-anterior=${!!antiga && antiga.includes('anterior')}`);
    }

    // ---- mensagens de varias linhas --------------------------------------------
    //
    // O que se mede NAO e o texto sobreviver a ida e volta -- isso e JSON, e passaria mesmo
    // com o desenho errado. Mede-se o que se VE: em HTML uma nova linha e apenas um espaco,
    // portanto sem `white-space: pre-wrap` a mensagem chegava certa e mostrava-se toda
    // seguida. E mede-se o TECTO no Rust, porque uma verificacao so na interface e uma
    // sugestao: chama-se o comando directamente, sem passar pelo guarda do JS.
    {
      const alvo = (vista.servidores || [])[0];
      const canal = alvo && (alvo.canais || []).find(c => c.tipo === 'texto');
      if (!alvo || !canal) {
        diz('ui varias linhas: sem servidor/canal para medir');
      } else {
        const chave = { servidor: alvo.id, canal: canal.id };
        const tresLinhas = 'primeira' + String.fromCharCode(10)
          + 'segunda' + String.fromCharCode(10) + 'terceira';
        await invoke('enviar', { ...chave, texto: tresLinhas }).catch(() => {});
        const msgs = await invoke('mensagens', chave).catch(() => []);
        const guardada = (msgs[msgs.length - 1] || {}).texto === tresLinhas;

        // E o DESENHO. Uma linha de altura contra tres.
        escolherServidor(alvo.id);
        escolherCanal(canal.id);
        await new Promise(r => setTimeout(r, 400));
        const ps = [...document.querySelectorAll('#stream .msg p')];
        const ultimo = ps[ps.length - 1];
        const estilo = ultimo && getComputedStyle(ultimo);
        const alturaLinha = estilo ? parseFloat(estilo.lineHeight) || 20 : 20;
        const altura = ultimo ? ultimo.getBoundingClientRect().height : 0;

        // O tecto, sem passar pelo guarda do JS.
        let recusou = false;
        try {
          await invoke('enviar', { ...chave, texto: 'x'.repeat(MAX_TEXTO + 1) });
        } catch (e) { recusou = true; }
        let aceitouNoLimite = false;
        try {
          await invoke('enviar', { ...chave, texto: 'y'.repeat(MAX_TEXTO) });
          aceitouNoLimite = true;
        } catch (e) { /* fica falso */ }

        diz(`ui varias linhas: guardada=${guardada}`
          + ` pre-wrap=${estilo ? estilo.whiteSpace : 'sem-elemento'}`
          + ` altura=${Math.round(altura)}px linha=${Math.round(alturaLinha)}px`
          + ` desenha-tres-linhas=${altura > alturaLinha * 2.4}`
          + ` rust-recusou-acima=${recusou} rust-aceitou-no-limite=${aceitouNoLimite}`);
      }
    }

    // ---- ler com a janela atras nao conta como ler -----------------------------
    //
    // O redesenho corre a cada mensagem que chega, esteja eu a olhar ou nao. Marcar sempre
    // como lido fazia a app dar por vista uma mensagem que ninguem viu -- e, pior, o aviso
    // do sistema nunca chegava a existir: quando o `talvezAvisar` olhava, a contagem ja
    // tinha voltado a zero. O caso em que um aviso serve era exactamente o que nao cobria.
    {
      const alvo = (vista.servidores || [])[0];
      const canal = alvo && (alvo.canais || []).find(c => c.tipo === 'texto');
      if (!alvo || !canal) {
        diz('ui ler com a janela atras: sem servidor/canal para medir');
      } else {
        const chave = { servidor: alvo.id, canal: canal.id };
        const guardado = janelaComFoco;

        const base = await invoke('marcar_lido', { ...chave, marcar: false }).catch(() => -1);

        // ESCOLHER O CANAL PRIMEIRO.
        //
        // Eu escolhia um servidor e um canal para a medicao e depois chamava
        // `desenharMensagens()`, que desenha o que esta ACTUAL -- que a esta altura do
        // guiao podia ser o modo privado, e nesse caso nem sequer passa pelo
        // `escreverMensagens`. A medicao dizia `null` e eu ia culpar a correccao.
        fecharDefinicoes();
        // E ter la ALGUMA COISA: com o canal vazio, o `desenharMensagens` desenha o estado
        // "ainda nao ha nada aqui" e volta ANTES do `escreverMensagens`. A medicao dizia
        // `null` e nao era a correccao que estava em causa -- era o caminho nunca ter sido
        // percorrido. Terceira vez hoje que um teste nao chega ao codigo que julga medir.
        await invoke('enviar', {
          servidor: alvo.id, canal: canal.id, texto: 'para haver o que redesenhar',
        }).catch(() => {});
        escolherServidor(alvo.id);
        escolherCanal(canal.id);
        await new Promise(r => setTimeout(r, 400));

        // A DECISAO: o redesenho pede para marcar exactamente quando a janela esta a frente.
        janelaComFoco = false;
        ultimoMarcarPedido = null;
        await desenharMensagens();
        const pediuComJanelaAtras = ultimoMarcarPedido;

        janelaComFoco = true;
        ultimoMarcarPedido = null;
        await desenharMensagens();
        const pediuComJanelaAFrente = ultimoMarcarPedido;

        janelaComFoco = guardado;
        diz(`ui ler com a janela atras: leu-base=${base >= 0}`
          + ` atras-pediu=${pediuComJanelaAtras}`
          + ` a-frente-pediu=${pediuComJanelaAFrente}`
          + ` decide-pelo-foco=${pediuComJanelaAtras === false && pediuComJanelaAFrente === true}`);
      }
    }

    // ---- o que o aviso do sistema deixa sair -----------------------------------
    //
    // A promessa e "por omissao, o texto da mensagem NAO vai para o aviso do Windows".
    // Mede-se a decisao, e nao o Windows: se dependesse de um aviso aparecer no ecra, nao
    // corria em CI e nao provava nada.
    {
      const antes = localStorage.getItem(AVISOS_TEXTO);
      const segredo = 'ISTO-NAO-PODE-SAIR';

      localStorage.removeItem(AVISOS_TEXTO);              // o estado de fabrica
      const porOmissao = corpoDoAviso('#geral', segredo);
      localStorage.setItem(AVISOS_TEXTO, '0');            // desligado a mao
      const desligado = corpoDoAviso('#geral', segredo);
      localStorage.setItem(AVISOS_TEXTO, '1');            // ligado de propria vontade
      const ligado = corpoDoAviso('#geral', segredo);

      if (antes === null) localStorage.removeItem(AVISOS_TEXTO);
      else localStorage.setItem(AVISOS_TEXTO, antes);

      diz(`ui aviso privacidade: por-omissao-esconde=${!porOmissao.includes(segredo)}`
        + ` desligado-esconde=${!desligado.includes(segredo)}`
        + ` ligado-mostra=${ligado === segredo}`
        + ` diz-onde=${porOmissao.includes('#geral')}`);
    }

    // ---- convites com veneno lá dentro (NO FIM, e de propósito) -----------------
    //
    // No fim porque um convite envenenado que passe deixa um servidor a mais no estado, e
    // isso contaminaria todas as medições que viessem a seguir -- eu deixaria de saber se
    // uma falha era da coisa medida ou do meu próprio ataque.
    //
    // E o que se mede é o DISCO, não o erro do comando. Sem a validação o comando TAMBÉM dá
    // erro -- «não consegui ligar» -- só que já criou o ficheiro onde o convite mandou.
    // Quem lesse o erro concluía «recusado» e estava enganado.
    {
      const venenos = JSON.parse(await invoke('convites_de_teste').catch(() => '[]'));
      const tentados = [];
      for (const [nome, codigo] of venenos) {
        try { await invoke('entrar_com_convite', { codigo }); tentados.push(`${nome}=aceite`); }
        catch (e) { tentados.push(`${nome}=erro`); }
      }
      // As escritas do log não são todas síncronas: da primeira vez o ficheiro só apareceu
      // ao fim de ~23 segundos, muito depois do comando ter voltado. Uma verificação
      // imediata dizia «sem rasto» com o ataque a caminho.
      await new Promise(r => setTimeout(r, 8000));
      const rasto = await invoke('escapou_alguma_coisa').catch(e => 'nao-medido:' + e);
      diz(`ui convite venenoso: ${venenos.length} tentados, rasto=${rasto}`);
    }

    document.querySelector('[data-modo="pers"]').click();
    $('#btn-qualidade').click();
  } else {
    diz('ui seletor: NAO ABRIU');
  }
  fechar('veu-fontes');

  // ---- O MICROFONE HONESTO (#35, #105, #106, #164, #191) ----
  //
  // Tudo isto vive em caminhos que o par legitimo nao exercita: no par nada falha, nada
  // desaparece e ninguem mexe num volume. Mede-se aqui, contra as funcoes a serio.
  // TUDO O QUE ESTE BLOCO SUJA, CAPTURADO ANTES DE O SUJAR.
  //
  // A reposicao era inline e o unico `catch` nao repunha nada: bastava o
  // `await desenharRodape()` rejeitar -- e ele espera pelo Rust a meio -- para a app ficar
  // com `voz.canal = 'medicao'` (um canal que nao existe) e `voz.micro` a ser um objecto
  // sem `getTracks()`, que faz o `sairDeVoz` rebentar. Uma medicao que estraga a app que
  // esta a medir e pior do que nao medir.
  const antes = {
    ruido: ruidoSuprimido,
    ruidoNoDisco: localStorage.getItem(RUIDO),
    microNoDisco: localStorage.getItem(MICROFONE),
    volumesNoDisco: localStorage.getItem(VOLUMES),
    recuado: microfoneRecuado,
    micro: voz.micro,
    canal: voz.canal,
    acimaDoChao: acimaDoChaoEm,
    eu: voz.eu,
    teste: testeDoMicro,
  };
  try {
    // #35/#191: persiste, e o pedido honra o que ficou guardado.
    // `guardado-e-lido` era uma tautologia: lia `typeof pedido.audio === 'object'`, que e
    // verdade sempre. O que interessa e se o valor DO DISCO chega a variavel -- e isso e
    // uma linha que so corre no arranque, portanto testa-se a expressao dela.
    const ruidoAntes = ruidoSuprimido;
    localStorage.setItem(RUIDO, '0');
    const leDoDiscoDesligado = localStorage.getItem(RUIDO) !== '0';
    localStorage.setItem(RUIDO, '1');
    const leDoDiscoLigado = localStorage.getItem(RUIDO) !== '0';
    localStorage.removeItem(RUIDO);
    const semDiscoFicaLigado = localStorage.getItem(RUIDO) !== '0';
    ruidoSuprimido = false;
    const semRuido = pedidoDeMicrofone('');
    ruidoSuprimido = true;
    const comRuido = pedidoDeMicrofone('');
    localStorage.setItem(RUIDO, ruidoAntes ? '1' : '0');
    ruidoSuprimido = ruidoAntes;

    // #105: o deviceId entra como `exact`, e so quando ha um escolhido.
    const comId = pedidoDeMicrofone('xpto-123');
    const semId = pedidoDeMicrofone('');

    diz(`ui microfone pedido: disco-0-da-desligado=${leDoDiscoDesligado === false}`
      + ` disco-1-da-ligado=${leDoDiscoLigado === true}`
      + ` sem-disco-fica-ligado=${semDiscoFicaLigado === true}`
      + ` desligado-pede-false=${semRuido.audio.noiseSuppression === false}`
      + ` ligado-pede-true=${comRuido.audio.noiseSuppression === true}`
      + ` ganho-automatico-no-pedido=${'autoGainControl' in comRuido.audio}`
      + ` id-exacto=${comId.audio.deviceId && comId.audio.deviceId.exact === 'xpto-123'}`
      + ` sem-escolha-sem-id=${!('deviceId' in semId.audio)}`);

    // #105: o RECUO. Um dispositivo que ja nao existe nao pode trocar «o microfone errado»
    // por «microfone nenhum» -- e e isso que o `exact` faz sozinho.
    const escolhaAntes = localStorage.getItem(MICROFONE) || '';
    localStorage.setItem(MICROFONE, 'este-dispositivo-nao-existe');
    microfoneRecuado = null;
    let recuou = 'sem-microfone-nesta-maquina';
    try {
      const m = await abrirMicrofone();
      recuou = `${microfoneRecuado === 'este-dispositivo-nao-existe'}`
        + ` faixas=${m.getAudioTracks().length}`;
      m.getTracks().forEach(t => t.stop());
    } catch (e) {
      recuou = `nao-abriu(${e && e.name ? e.name : e})`;
    }
    localStorage.setItem(MICROFONE, escolhaAntes);
    diz(`ui microfone recuo: ${recuou}`);

    // #164: o volume por pessoa, e o limitador que so entra acima de 100%.
    const CHV = 'medicao-do-volume';
    const v = vozDe(CHV);
    guardarVolume(CHV, 0.5);
    const meio = { ganho: v.ganho.gain.value, comp: !!v.comp };
    guardarVolume(CHV, 2);
    const dobro = { ganho: v.ganho.gain.value, comp: !!v.comp };
    guardarVolume(CHV, 1);
    const normal = { ganho: v.ganho.gain.value, comp: !!v.comp };
    const guardouOnormal = CHV in volumesGuardados();
    // E um valor estragado no disco -- de uma mexida a mao ou de uma versao futura -- nao
    // pode chegar ao `GainNode`: 900 num ganho nao e um volume alto, e um estouro.
    const volAntes = localStorage.getItem(VOLUMES);
    localStorage.setItem(VOLUMES, '{"a":900,"b":"alto","c":-3,"d":1.5}');
    volumesEmMemoria = null;
    const filtrados = [volumeDe('a'), volumeDe('b'), volumeDe('c'), volumeDe('d')];
    localStorage.setItem(VOLUMES, '{ isto nao e json');
    volumesEmMemoria = null;
    const comLixo = volumeDe('a');
    if (volAntes === null) localStorage.removeItem(VOLUMES);
    else localStorage.setItem(VOLUMES, volAntes);
    volumesEmMemoria = null;
    // E o silenciar continua a ganhar ao volume: quem esta calado fica calado.
    voz.silenciados.add(CHV);
    guardarVolume(CHV, 2);
    const silenciado = v.ganho.gain.value;
    voz.silenciados.delete(CHV);
    guardarVolume(CHV, 1);
    calarPeer(CHV);
    cortesDaVoz.delete(CHV);
    diz(`ui volume: metade=${meio.ganho} sem-limitador=${!meio.comp}`
      + ` dobro=${dobro.ganho} com-limitador=${dobro.comp}`
      + ` normal=${normal.ganho} limitador-saiu=${!normal.comp}`
      + ` normal-nao-se-guarda=${!guardouOnormal}`
      + ` disco-estragado-filtrado=${JSON.stringify(filtrados)}`
      + ` json-partido-nao-rebenta=${comLixo === 1}`
      + ` silenciado-ganha=${silenciado === 0}`);

    // #106: a marca do microfone que nao capta -- MEDIDA PELO CAMINHO QUE A PRODUZ.
    //
    // A versao anterior desta medicao punha o relogio a mao e chamava o `desenharRodape`.
    // Media a CONDICAO e nao o caminho -- e por isso passava a verde por cima de um defeito
    // grave: o escritor em `medirFala` repunha o relogio com a MESMA janela que o leitor
    // testava, oito vezes por segundo, e a marca so podia acender durante ~120 ms em cada
    // 15 s. Um microfone morto dava um aviso a piscar, e a medicao dizia que estava bem.
    //
    // Agora passa-se pelo `medirFala` a serio, com analisadores a serio: um em silencio
    // (nada ligado -- da zeros) e um com um oscilador (da energia a valer). O que se exige
    // e a propriedade que faltava: COM SILENCIO, O ESCRITOR NAO PODE MEXER NO RELOGIO.
    const micAntes = voz.micro;
    const canalAntes = voz.canal;
    const euAntes = voz.eu;
    const analisadorAntes = voz.analisadores.get(voz.eu);
    const ctxM = contextoDeAudio();
    const mudo = ctxM.createAnalyser();
    mudo.fftSize = 512;
    const alto = ctxM.createAnalyser();
    alto.fftSize = 512;
    const osc = ctxM.createOscillator();
    osc.connect(alto);          // so ao analisador: nao vai as colunas
    osc.start();
    voz.micro = { getAudioTracks: () => [{ enabled: true }] };
    voz.canal = canalAntes || 'medicao';
    voz.eu = euAntes || 'medicao-de-mim';
    voz.analisadores.set(voz.eu, { an: mudo, fonte: null, dados: new Float32Array(512) });
    acimaDoChaoEm = performance.now() - (JANELA_DO_PICO + 1000);
    const relogioAntes = acimaDoChaoEm;
    // Oito voltas de `medirFala` com silencio -- as mesmas que davam num segundo real.
    for (let i = 0; i < 8; i += 1) medirFala();
    const relogioIntacto = acimaDoChaoEm === relogioAntes;
    // O `desenharRodape` e ASSINCRONO -- ele espera pelo `qualidadeDaLigacao`, que vai ao
    // Rust, antes de chegar ao botao do microfone. Sem este `await` lia-se o botao antes de
    // ele ter sido tocado.
    await desenharRodape();
    const marcou = $('#btn-mic').classList.contains('is-avisado');
    const disse = ($('#btn-mic').title || '').includes('não capta nada');
    // E o contra-exemplo, tambem pelo caminho a serio: com energia, o escritor mexe no
    // relogio e a marca sai.
    voz.analisadores.set(voz.eu, { an: alto, fonte: null, dados: new Float32Array(512) });
    await new Promise(r => setTimeout(r, 60));   // o oscilador tem de encher o analisador
    medirFala();
    const relogioAndou = acimaDoChaoEm > relogioAntes;
    await desenharRodape();
    const semMarca = !$('#btn-mic').classList.contains('is-avisado');
    try { osc.stop(); osc.disconnect(); } catch (e) { /* ja */ }
    try { mudo.disconnect(); alto.disconnect(); } catch (e) { /* ja */ }
    voz.falando.delete(voz.eu);
    if (analisadorAntes) voz.analisadores.set(euAntes, analisadorAntes);
    else voz.analisadores.delete(voz.eu);
    voz.micro = micAntes;
    voz.canal = canalAntes;
    voz.eu = euAntes;
    await desenharRodape();
    diz(`ui microfone mudo: silencio-nao-mexe-no-relogio=${relogioIntacto}`
      + ` marcou=${marcou} disse-porque=${disse}`
      + ` som-mexe-no-relogio=${relogioAndou} com-som-nao-marca=${semMarca}`);

    // #2 DA REVISAO: o guarda das definicoes. Era
    // `$('#defs').classList.contains('is-on')` -- uma classe que NINGUEM poe -- portanto
    // sempre falso, e o `finally` do teste do microfone nunca redesenhava: todos os
    // caminhos de falha ficavam com «A gravar 3 segundos...» para sempre.
    const defsAntes = $('#defs').hidden;
    const painelAntes = painelActivo;
    $('#defs').hidden = true;
    const fechadoDizNao = aVerAsDefinicoesDaVoz();
    $('#defs').hidden = false;
    painelActivo = 'voz';
    const abertoNaVozDizSim = aVerAsDefinicoesDaVoz();
    painelActivo = 'conta';
    const noutroPainelDizNao = aVerAsDefinicoesDaVoz();
    $('#defs').hidden = defsAntes;
    painelActivo = painelAntes;
    diz(`ui definicoes da voz: fechado=${fechadoDizNao} aberto-na-voz=${abertoNaVozDizSim}`
      + ` noutro-painel=${noutroPainelDizNao}`);

    // #1 DA REVISAO, e era o pior: TROCAR DE MICROFONE dizia que o microfone tinha morrido.
    //
    // O laco de envio lia a variavel de MODULO `envio`, e o `comecarAEnviarVoz` novo corre
    // `pararDeEnviarVoz()` e chega a `envio = E2` sem um unico `await` pelo meio. Quando a
    // continuacao do laco antigo acordava, `envio` ja era o E2 com `vivo === true`, e ele
    // escrevia «o teu microfone deixou de entregar som» por cima do `vozFalhou = null` que
    // a reabertura acabara de limpar.
    //
    // Isto reproduz a troca com dois microfones a serio. O par legitimo nunca troca de
    // microfone, portanto nada disto era exercitado em lado nenhum.
    let m1 = null, m2 = null;
    let trocaLimpa = 'sem-microfone';
    try {
      m1 = await abrirMicrofone();
      m2 = await abrirMicrofone();
      vozFalhou = null;
      comecarAEnviarVoz(m1);
      const arrancou = !!envio;
      comecarAEnviarVoz(m2);
      // O `read()` do laco antigo so rejeita na volta seguinte do event loop.
      await new Promise(r => setTimeout(r, 400));
      trocaLimpa = `arrancou=${arrancou} sem-queixa=${vozFalhou === null}`
        + ` queixa="${(vozFalhou || '').slice(0, 40)}"`;
    } catch (e) {
      trocaLimpa = `nao-deu(${e && e.name ? e.name : e})`;
    } finally {
      pararDeEnviarVoz();
      if (m1) m1.getTracks().forEach(t => t.stop());
      if (m2) m2.getTracks().forEach(t => t.stop());
      vozFalhou = null;
    }
    diz(`ui trocar de microfone: ${trocaLimpa}`);

    // O ACHADO A, e era o pior de todos: SILENCIADO ANTES, SILENCIADO DEPOIS.
    //
    // O «silenciado» so vive no `faixa.enabled`, e as faixas de um `getUserMedia` novo
    // nascem sempre a `true`. Silenciava-me para tossir, ligava os auscultadores, e voltava
    // a transmitir sem o saber. Isto corre o `reabrirMicrofone` A SERIO -- e o par legitimo
    // nunca silencia nem troca de microfone, portanto nada disto era exercitado.
    const surdoAntes = surdo;
    const microAntes2 = voz.micro;
    const canalAntes2 = voz.canal;
    let calado = 'sem-microfone';
    try {
      voz.canal = canalAntes2 || 'medicao';
      voz.micro = await abrirMicrofone();
      voz.micro.getAudioTracks()[0].enabled = false;   // silenciei-me
      ultimaReabertura = 0;
      await reabrirMicrofone('medicao do silenciado', true);
      const depoisDeSilenciado = voz.micro.getAudioTracks()[0].enabled;
      // E o surdo: ficar a falar para uma chamada que nao se esta a ouvir e exactamente o
      // que o botao de silenciar tudo existe para impedir.
      voz.micro.getAudioTracks()[0].enabled = true;
      surdo = true;
      ultimaReabertura = 0;
      await reabrirMicrofone('medicao do surdo', true);
      const depoisDeSurdo = voz.micro.getAudioTracks()[0].enabled;
      surdo = false;
      // E o contra-exemplo: quem NAO estava silenciado continua a transmitir.
      voz.micro.getAudioTracks()[0].enabled = true;
      ultimaReabertura = 0;
      await reabrirMicrofone('medicao do normal', true);
      const depoisDeNormal = voz.micro.getAudioTracks()[0].enabled;
      calado = `silenciado-continua=${depoisDeSilenciado === false}`
        + ` surdo-nao-transmite=${depoisDeSurdo === false}`
        + ` normal-continua=${depoisDeNormal === true}`;
    } catch (e) {
      calado = `nao-deu(${e && e.name ? e.name : e})`;
    } finally {
      pararDeEnviarVoz();
      if (voz.micro && voz.micro !== microAntes2) {
        voz.micro.getTracks().forEach(t => t.stop());
      }
      pararDeVigiar(voz.eu);
      surdo = surdoAntes;
      voz.micro = microAntes2;
      voz.canal = canalAntes2;
      vozFalhou = null;
    }
    diz(`ui microfone silenciado: ${calado}`);

    // #107: o TESTE a serio, do principio ao fim. Grava 3 s, passa-os pelo Opus e toca-os
    // -- o mesmo circuito que o `--autoteste` ja tinha e que ninguem conseguia alcancar.
    // O que se exige daqui nao e «passou»: e que diga QUAL das tres metades falhou, porque
    // «o microfone nao funciona» sem dizer qual e o mesmo que nao dizer nada.
    // Primeiro a RECUSA: com uma chamada aberta o teste tem de se recusar a correr, senao a
    // gravacao sai pelas colunas e entra pelo microfone DA CHAMADA.
    const canalDoTeste = voz.canal;
    voz.canal = 'medicao';
    testeDoMicro = null;
    await testarMicrofone();
    const recusou = !!testeDoMicro && testeDoMicro.estado === 'mau'
      && testeDoMicro.texto.includes('Sai da chamada');
    voz.canal = canalDoTeste;
    testeDoMicro = null;

    // A REENTRANCIA, medida a serio. Antes lia-se `!jaCorria && !testeACorrer` com os dois
    // a serem lidos quando nada estava a correr -- `true` em qualquer universo, mesmo com a
    // guarda apagada. Agora chamam-se DUAS ao mesmo tempo e le-se o que a segunda devolve:
    // `false` e a guarda a dizer que recusou.
    const primeira = testarMicrofone();
    const durante = testeACorrer;
    const segunda = await testarMicrofone();
    const correu = await primeira;
    const r = testeDoMicro || {};
    const nomeia = typeof r.texto === 'string' && (
      r.texto.startsWith('O teu microfone') || r.texto.startsWith('O dispositivo')
      || r.texto.startsWith('O codec desta máquina') || r.texto.startsWith('Captou'));
    diz(`ui teste do microfone: recusa-com-chamada=${recusou}`
      + ` estado=${r.estado} nomeia-a-metade=${nomeia}`
      + ` marcou-a-correr=${durante} segunda-recusou=${segunda === false}`
      + ` primeira-correu=${correu === true}`
      + ` texto="${(r.texto || '').slice(0, 60)}"`);
  } catch (e) {
    diz(`ui microfone: REBENTOU ${e && e.message ? e.message : e}`);
  } finally {
    ruidoSuprimido = antes.ruido;
    if (antes.ruidoNoDisco === null) localStorage.removeItem(RUIDO);
    else localStorage.setItem(RUIDO, antes.ruidoNoDisco);
    if (antes.microNoDisco === null) localStorage.removeItem(MICROFONE);
    else localStorage.setItem(MICROFONE, antes.microNoDisco);
    if (antes.volumesNoDisco === null) localStorage.removeItem(VOLUMES);
    else localStorage.setItem(VOLUMES, antes.volumesNoDisco);
    volumesEmMemoria = null;
    microfoneRecuado = antes.recuado;
    voz.micro = antes.micro;
    voz.canal = antes.canal;
    acimaDoChaoEm = antes.acimaDoChao;
    voz.eu = antes.eu;
    testeDoMicro = antes.teste;
    voz.silenciados.delete('medicao-do-volume');
    calarPeer('medicao-do-volume');
    cortesDaVoz.delete('medicao-do-volume');
    await desenharRodape().catch(() => {});
  }

  // ---- A FOLGA ADAPTATIVA, MEDIDA NO CODIGO A SERIO (#65, #104, #117) ----
  //
  // Isto chama o `tocar()` DE PRODUCAO, com `AudioData` a serio, e le o mesmo
  // `cortesDaVoz` que o painel le. Nao ha copia da logica aqui: uma copia provaria que a
  // copia funciona. Nao ha rede nenhuma envolvida -- o par legitimo nunca corta, e por
  // isso nao exercita nada disto.
  try {
    const CH = 'medicao-da-folga';
    const amostra = () => new AudioData({
      format: 'f32-planar', sampleRate: VOZ_HZ, numberOfFrames: 960,
      numberOfChannels: 1, timestamp: 0, data: new Float32Array(960),
    });
    const v = vozDe(CH);
    const c = v.corte;
    const ctx = v.ctx;

    // 1. O PRIMEIRO PEDACO NAO E UM CORTE. Com `proximo` a zero a condicao e sempre
    //    verdadeira, e conta-la dava um corte a toda a gente que entra numa sala.
    v.proximo = 0;
    tocar(CH, amostra());
    const primeiro = c.total;

    // 2. Dois cortes na mesma janela sobem UM degrau.
    for (let i = 0; i < 2; i += 1) {
      v.proximo = 0.001;  // atras do relogio: e um corte
      tocar(CH, amostra());
    }
    const depoisDeDois = { total: c.total, folga: c.folga };

    // 3. Um terceiro corte logo a seguir NAO sobe outra vez: uma rajada e UMA avaria.
    v.proximo = 0.001;
    tocar(CH, amostra());
    const depoisDeTres = { total: c.total, folga: c.folga };

    // 4. A DESCIDA. Finge-se um minuto inteiro limpo e toca-se dentro da janela boa.
    //    O 0.3 e o MEIO da janela [+0.01, +0.6] de proposito: com +0.1, o relogio do
    //    contexto avancava o suficiente entre esta linha e a leitura la dentro para o
    //    valor cair fora, e o `tocar` seguia pelo ramo do CORTE -- a medicao dizia «nao
    //    desceu» sobre uma descida que nunca foi pedida.
    c.limpoDesde = performance.now() - (LIMPO_PARA_DESCER + 1000);
    const antesDoRelogio = ctx.currentTime;
    v.proximo = antesDoRelogio + 0.3;
    tocar(CH, amostra());
    const depoisDeDescer = c.folga;
    const cortesNaDescida = c.total;

    // 5. E a descida PARA no chao: um segundo minuto limpo nao a leva abaixo dos 80 ms.
    c.limpoDesde = performance.now() - (LIMPO_PARA_DESCER + 1000);
    v.proximo = ctx.currentTime + 0.3;
    tocar(CH, amostra());
    const noChao = c.folga;

    // 6. E o tecto: mesmo com cortes a mais, nunca passa dos 200 ms.
    for (let i = 0; i < 40; i += 1) {
      c.subiuEm = 0;
      c.quando = [performance.now(), performance.now()];
      v.proximo = 0.001;
      tocar(CH, amostra());
    }
    const noTecto = c.folga;

    // 7. OS LIMITES, e nao so as guardas que os precedem.
    //
    //    Com os valores de hoje (80 ms + passos de 20 ms ate 200) a folga cai SEMPRE em
    //    cima do tecto e do chao, portanto as guardas `folga < TECTO` e `folga > FOLGA`
    //    ja param tudo e os `Math.min`/`Math.max` nunca chegam a morder. Isso descobriu-se
    //    a sabotar: tirar qualquer um dos dois nao mudava um unico numero medido.
    //
    //    Um limite que nunca se exercita e um limite sobre o qual nao se sabe nada -- e
    //    basta mudar o passo para 30 ms para ele passar a ser o unico a segurar. Por isso
    //    poe-se a folga onde UM PASSO a atira para fora: 190 ms sobe para 210 (fica 200),
    //    90 ms desce para 70 (fica 80).
    c.folga = 0.19;
    c.subiuEm = 0;
    c.quando = [performance.now(), performance.now()];
    v.proximo = 0.001;
    tocar(CH, amostra());
    const tectoAserio = c.folga;

    c.folga = 0.09;
    c.limpoDesde = performance.now() - (LIMPO_PARA_DESCER + 1000);
    v.proximo = ctx.currentTime + 0.3;
    tocar(CH, amostra());
    const chaoAserio = c.folga;

    diz(`ui folga: primeiro-nao-conta=${primeiro === 0}`
      + ` dois-cortes=${depoisDeDois.total} subiu-para=${Math.round(depoisDeDois.folga * 1000)}ms`
      + ` terceiro-nao-sobe=${depoisDeTres.folga === depoisDeDois.folga}`
      + ` desceu-para=${Math.round(depoisDeDescer * 1000)}ms`
      + ` descida-sem-corte=${cortesNaDescida === depoisDeTres.total}`
      + ` chao=${Math.round(noChao * 1000)}ms`
      + ` tecto-por-cima=${Math.round(tectoAserio * 1000)}ms`
      + ` chao-por-baixo=${Math.round(chaoAserio * 1000)}ms`
      + ` tecto=${Math.round(noTecto * 1000)}ms`
      + ` saltado=${c.saltado.toFixed(2)}s`);

    calarPeer(CH);
    cortesDaVoz.delete(CH);
  } catch (e) {
    diz(`ui folga: REBENTOU ${e && e.message ? e.message : e}`);
  }
})();
