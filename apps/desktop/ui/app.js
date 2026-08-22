/* ==========================================================================
   Bruma — interface.
   Nenhuma chave privada passa por aqui: o JavaScript pede ações, o Rust assina e cifra.
   ========================================================================== */

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (s, r = document) => r.querySelector(s);
const $$ = (s, r = document) => [...r.querySelectorAll(s)];

let vista = null;        // o último estado vindo do Rust
let servidorAtual = null;
let canalAtual = null;
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

function desenharRail() {
  const rail = $('#rail-servidores');
  rail.textContent = '';
  for (const s of vista.servidores) {
    const b = elemento('button', 'rail__pill', s.nome.slice(0, 2).toUpperCase());
    b.dataset.tip = s.nome;
    if (s.id === servidorAtual) b.classList.add('is-active');
    b.onclick = () => escolherServidor(s.id);
    rail.append(b);
  }
}

function servidor() {
  return vista.servidores.find(s => s.id === servidorAtual) || null;
}

function desenharCanais() {
  const lista = $('#lista-canais');
  lista.textContent = '';
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
    bloco.append(elemento('i', null, m.fundador ? 'fundou este servidor' : chaveCurta(m.chave)));
    linha.append(av, bloco);
    lista.append(linha);
  }
}

async function desenharMensagens() {
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

  let anterior = null;
  for (const m of msgs) {
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
      const d = new Date(m.ts_ms);
      cab.append(elemento('time', null,
        `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`));
      corpo.append(cab);
    }
    corpo.append(elemento('p', null, m.texto));
    art.append(corpo);
    stream.append(art);
    anterior = m;
  }
  stream.scrollTop = stream.scrollHeight;
}

function desenharTopo() {
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
  $('#meu-nome').textContent = vista.nome || 'sem nome';
  $('#minha-chave').textContent = chaveCurta(vista.chave);
  pintar($('#meu-avatar'), vista.chave);

  if (!vista.servidores.some(s => s.id === servidorAtual)) {
    servidorAtual = vista.servidores[0] ? vista.servidores[0].id : null;
    canalAtual = null;
  }
  const s = servidor();
  if (s && !s.canais.some(c => c.id === canalAtual)) {
    const primeiro = s.canais.find(c => c.tipo === 'texto') || s.canais[0];
    canalAtual = primeiro ? primeiro.id : null;
  }

  desenharRail();
  desenharCanais();
  desenharMembros();
  desenharTopo();
  await desenharMensagens();
  desenharRodape();
}

function escolherServidor(id) {
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

$('#btn-perfil').onclick = () => {
  $('#in-nome').value = vista.nome || '';
  erroEm('erro-nome', '');
  abrir('veu-bemvindo');
  $('#in-nome').focus();
};
$('#ok-nome').onclick = async () => {
  const nome = $('#in-nome').value.trim();
  if (!nome) return erroEm('erro-nome', 'escreve um nome');
  try {
    await invoke('definir_nome', { nome });
    fechar('veu-bemvindo');
    await desenharTudo();
  } catch (e) { erroEm('erro-nome', String(e)); }
};

$('#entrada').addEventListener('keydown', async ev => {
  if (ev.key !== 'Enter' || !ev.target.value.trim()) return;
  const texto = ev.target.value;
  ev.target.value = '';
  try {
    await invoke('enviar', { servidor: servidorAtual, canal: canalAtual, texto });
    await desenharMensagens();
  } catch (e) { console.error(e); }
});

/* ---------- eventos vindos do núcleo ---------- */

listen('servidor-mudou', async ev => {
  await desenharTudo();
  // O chat da sala vive na coluna da direita, fora da vista de canal: se estivermos a
  // ler um canal de texto, o desenharTudo não lhe toca e as mensagens novas não apareciam.
  await desenharChatDaSala();
});
listen('peer-ligado', () => { ligados += 1; desenharTopo(); });
listen('peer-desligado', () => { ligados = Math.max(0, ligados - 1); desenharTopo(); });

/* ---------- explicações: o porquê vive na app ---------- */

const EXPLICACOES = {
  identidade: {
    titulo: 'A tua identidade',
    corpo: [
      'Foi criada neste computador na primeira vez que abriste a app. É uma chave, e é ao mesmo tempo o teu ID e o teu endereço na rede.',
      'Não existe conta, não existe registo, e ninguém — nem tu — a pode recuperar se apagares a pasta de dados.',
    ],
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

async function procurarAtualizacao() {
  try {
    const { check } = window.__TAURI__.updater;
    const nova = await check();
    if (!nova) return;
    $('#texto-update').textContent = `Há uma versão nova do Bruma (${nova.version}).`;
    $('#faixa-update').hidden = false;
    $('#adiar-update').onclick = () => { $('#faixa-update').hidden = true; };
    $('#btn-update').onclick = async () => {
      $('#btn-update').disabled = true;
      $('#texto-update').textContent = 'A descarregar…';
      try {
        await nova.downloadAndInstall();
        await window.__TAURI__.process.relaunch();
      } catch (e) {
        $('#texto-update').textContent = `Não consegui atualizar: ${e}`;
        $('#btn-update').disabled = false;
      }
    };
  } catch (e) {
    // Sem rede, ou o endpoint em baixo. Não vale a pena incomodar ninguém com isso.
    console.warn('verificação de atualização falhou:', e);
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
  }
  if (canal && canal.dataset.canal) {
    const id = canal.dataset.canal;
    if (itens.length) itens.push('-');
    itens.push({
      rotulo: 'Apagar canal', perigo: true,
      accao: () => invoke('apagar_canal', { servidor: servidorAtual, canal: id }).catch(console.error),
    });
  }
  if (servidorAtual && !canal && !msg && !membro) {
    itens.push({ rotulo: 'Convidar alguém', accao: () => $('#btn-convite').click() });
  }
  if (itens.length) itens.push('-');
  itens.push({ rotulo: 'Servidores de ligação…', accao: abrirDefinicoesDeRede });

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
  pcs: new Map(),        // peer -> ligação
  presentes: new Map(),  // peer -> canal em que está
  falando: new Set(),    // quem está a emitir som agora
  silenciados: new Set(),// pessoas silenciadas uma a uma
  aPartilhar: new Set(), // quem está a transmitir o ecrã
  aVer: null,            // de quem estou a ver a transmissão
  aSerVistoPor: new Set(), // quem pediu para ver o MEU ecrã — só a esses se envia
  analisadores: new Map(),
  audioCtx: null,
};

function servidoresDeGelo() {
  const bruto = (localStorage.getItem('bruma.ice') || '').trim();
  if (!bruto) return [];
  return bruto.split('\n').map(l => l.trim()).filter(Boolean).map(l => {
    // turn:utilizador:segredo@host:porta  ->  { urls, username, credential }
    const m = l.match(/^(turns?):([^:@]+):([^@]+)@(.+)$/i);
    if (m) return { urls: `${m[1]}:${m[4]}`, username: m[2], credential: m[3] };
    return { urls: l };
  });
}

function abrirDefinicoesDeRede() {
  $('#in-ice').value = localStorage.getItem('bruma.ice') || '';
  erroEm('erro-rede', '');
  abrir('veu-rede');
}
$('#fechar-rede').onclick = () => fechar('veu-rede');
$('#ok-rede').onclick = () => {
  localStorage.setItem('bruma.ice', $('#in-ice').value.trim());
  fechar('veu-rede');
  desenharVoz();
};

async function entrarEmVoz(servidor, canal) {
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
      navigator.mediaDevices.getUserMedia({
        audio: { echoCancellation: true, noiseSuppression: true },
      }),
      new Promise((_, rej) => setTimeout(() => rej(new Error('sem resposta ao pedido')), 20000)),
    ]);
    if (voz.canal !== canal) {          // saiu enquanto se esperava
      voz.micro.getTracks().forEach(t => t.stop());
      voz.micro = null;
      return;
    }
    // O microfone chegou depois das ligacoes: junta-se a elas.
    for (const [, l] of voz.pcs) {
      voz.micro.getTracks().forEach(t => l.pc.addTrack(t, voz.micro));
    }
    vigiarAudio(voz.eu, voz.micro);
    desenharVoz();
  } catch (e) {
    // Sem microfone continua a dar para ouvir e para partilhar ecra.
    console.warn('sem microfone:', e);
    voz.micro = null;
  }
  // Quem já lá estava: liga-se agora.
  for (const [peer, c] of voz.presentes) {
    if (c === canal) garantirLigacao(peer);
  }
  desenharVoz();
}

async function sairDeVoz(anunciar = true) {
  if (anunciar && voz.canal) {
    await invoke('presenca_de_voz', { servidor: voz.servidor, canal: null }).catch(() => {});
  }
  for (const [, l] of voz.pcs) l.pc.close();
  voz.pcs.clear();
  if (voz.micro) voz.micro.getTracks().forEach(t => t.stop());
  if (voz.ecra) voz.ecra.getTracks().forEach(t => t.stop());
  if (voz.camara) voz.camara.getTracks().forEach(t => t.stop());
  for (const chave of [...voz.analisadores.keys()]) pararDeVigiar(chave);
  voz.falando.clear();
  voz.aPartilhar.clear();
  voz.aVer = null;
  voz.micro = null; voz.ecra = null; voz.camara = null;
  voz.canal = null;
  desenharVoz();
  desenharRodape();
}

function garantirLigacao(peer) {
  if (voz.pcs.has(peer)) return voz.pcs.get(peer);
  const pc = new RTCPeerConnection({ iceServers: servidoresDeGelo() });
  // "Negociação perfeita": quem tem o identificador maior cede em caso de choque.
  // Sem isto, duas ofertas em simultâneo deixam a ligação num estado impossível.
  const l = { pc, educado: voz.eu > peer, aFazerOferta: false, ignorarOferta: false, stream: null };
  voz.pcs.set(peer, l);

  if (voz.micro) voz.micro.getTracks().forEach(t => pc.addTrack(t, voz.micro));
  if (voz.ecra) voz.ecra.getTracks().forEach(t => pc.addTrack(t, voz.ecra));
  if (voz.camara) voz.camara.getTracks().forEach(t => pc.addTrack(t, voz.camara));

  pc.onicecandidate = e => {
    if (e.candidate) sinalizar(peer, { tipo: 'ice', candidato: e.candidate });
  };
  pc.ontrack = e => {
    l.stream = e.streams[0];
    vigiarAudio(peer, l.stream);
    desenharVoz();
  };
  pc.onconnectionstatechange = () => {
    if (pc.connectionState === 'failed' || pc.connectionState === 'closed') {
      voz.pcs.delete(peer);
      desenharVoz();
    }
  };
  pc.onnegotiationneeded = async () => {
    try {
      l.aFazerOferta = true;
      await pc.setLocalDescription();
      sinalizar(peer, { tipo: 'sdp', sdp: pc.localDescription });
    } catch (e) {
      console.error(e);
    } finally {
      l.aFazerOferta = false;
    }
  };
  // Um peer que chega tem de saber o que ja estou a enviar.
  setTimeout(() => sinalizar(peer, { tipo: 'estado', ecra: !!voz.ecra, camara: !!voz.camara }), 400);
  return l;
}

function sinalizar(peer, dados) {
  invoke('enviar_sinal', {
    para: peer, servidor: voz.servidor, canal: voz.canal, dados: JSON.stringify(dados),
  }).catch(console.error);
}

async function receberSinal(de, dados) {
  const l = garantirLigacao(de);
  const pc = l.pc;
  try {
    if (dados.tipo === 'sdp') {
      const desc = dados.sdp;
      const choque = desc.type === 'offer' && (l.aFazerOferta || pc.signalingState !== 'stable');
      l.ignorarOferta = !l.educado && choque;
      if (l.ignorarOferta) return;
      await pc.setRemoteDescription(desc);
      if (desc.type === 'offer') {
        await pc.setLocalDescription();
        sinalizar(de, { tipo: 'sdp', sdp: pc.localDescription });
      }
    } else if (dados.tipo === 'assistir') {
      if (dados.ligado) voz.aSerVistoPor.add(de); else voz.aSerVistoPor.delete(de);
      actualizarEspectadores();
      return;
    } else if (dados.tipo === 'estado') {
      if (dados.ecra) {
        voz.aPartilhar.add(de);
      } else {
        voz.aPartilhar.delete(de);
        // Parou de transmitir: o <video> dele fica sem fonte e a memoria com ele.
        fecharFluxoRecebido(de);
      }
      if (voz.aVer === de && !dados.ecra) voz.aVer = null;
      desenharVoz();
      return;
    } else if (dados.tipo === 'ice') {
      try {
        await pc.addIceCandidate(dados.candidato);
      } catch (e) {
        if (!l.ignorarOferta) throw e;
      }
    }
  } catch (e) {
    console.error('sinal:', e);
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
function fluxoDePedacos() {
  const media = new MediaSource();
  const el = document.createElement('video');
  el.autoplay = true;
  el.playsInline = true;
  el.muted = true;
  el.src = URL.createObjectURL(media);

  const fila = [];
  let buffer = null;
  let codec = null;
  let aberto = false;

  /* O codec não se assume, vem escrito no fluxo.
     O `addSourceBuffer` obriga a declará-lo, e o navegador VALIDA o que se lhe declara
     contra o cabeçalho: se não bater certo, recusa tudo com um "stream parsing failed"
     que não explica nada. Nesta máquina a NVIDIA produz Baseline 4.2; noutra placa será
     outro. Por isso espera-se por ele antes de abrir o buffer. */
  const abrir = () => {
    if (buffer || !aberto || !codec) return;
    const tipo = `video/mp4; codecs="${codec}"`;
    if (!window.MediaSource || !MediaSource.isTypeSupported(tipo)) {
      console.warn('esta webview não sabe descodificar', tipo);
      return;
    }
    try {
      buffer = media.addSourceBuffer(tipo);
      buffer.mode = 'sequence';
      buffer.addEventListener('updateend', escoar);
      escoar();
    } catch (e) {
      console.warn('não consegui abrir o buffer de vídeo:', e);
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
      // Se a fila crescer é porque o navegador não acompanha; nesse caso o que interessa
      // é o presente, não o passado.
      if (fila.length > 60) fila.splice(0, fila.length - 30);
      escoar();
    },
    fechar() {
      try { el.pause(); } catch (e) { /* já parado */ }
      try { URL.revokeObjectURL(el.src); } catch (e) { /* já libertado */ }
      el.removeAttribute('src');
      fila.length = 0;
    },
  };
}

async function alternarEcra() {
  if (voz.ecra) {
    await invoke('parar_de_partilhar').catch(() => {});
    if (voz.ecra.fechar) voz.ecra.fechar();
    voz.ecra = null;
    anunciarEstado();
    desenharVoz();
    desenharRodape();
    return;
  }
  if (!voz.canal || !voz.servidor) return;

  const fluxo = fluxoDePedacos();
  const canal = new window.__TAURI__.core.Channel();
  canal.onmessage = pedaco => fluxo.empurrar(pedaco);

  try {
    await invoke('comecar_a_partilhar', {
      servidor: voz.servidor,
      canalVoz: voz.canal,
      saida: canal,
    });
  } catch (e) {
    fluxo.fechar();
    console.warn('não consegui começar a partilhar:', e);
    return;
  }
  voz.ecra = fluxo;
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
      fluxo = fluxoDePedacos();
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
  const lista = [...voz.pcs.keys()].filter(p => voz.aSerVistoPor.has(p));
  invoke('definir_espectadores', { chaves: lista }).catch(() => {});
}

function nomeDoPeer(peer) {
  if (peer === voz.eu) return 'tu';
  const s = servidor();
  const m = s && s.membros.find(x => x.chave === peer);
  return m ? m.nome : `${peer.slice(0, 6)}…`;
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
  if (chave === voz.eu) return voz.ecra ? voz.ecra.el : null;
  const f = fluxosRecebidos.get(chave);
  return f ? f.el : null;
}

/** A câmara e o microfone continuam a vir do WebRTC. */
function fluxoDe(chave) {
  if (chave === voz.eu) return voz.camara || voz.micro;
  const l = voz.pcs.get(chave);
  return l ? l.stream : null;
}

function fecharFluxoRecebido(chave) {
  const f = fluxosRecebidos.get(chave);
  if (!f) return;
  f.fechar();
  fluxosRecebidos.delete(chave);
}

function painelDeVoz(chave, stream, opcoes = {}) {
  const t = elemento('div', 'tile');
  t.dataset.chave = chave;
  if (voz.falando.has(chave)) t.classList.add('a-falar');

  const transmite = voz.aPartilhar.has(chave) || (chave === voz.eu && !!voz.ecra);
  const aVer = opcoes.aVer;
  const temVideo = stream && stream.getVideoTracks().length > 0;

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
    const el = document.createElement('video');
    el.autoplay = true;
    el.playsInline = true;
    if (chave === voz.eu) { el.muted = true; el.dataset.proprio = '1'; }
    else el.muted = surdo || voz.silenciados.has(chave);
    el.srcObject = stream;
    t.append(el);
  } else {
    const sem = elemento('div', 'tile__sem-video');
    const av = elemento('span', 'ident');
    pintar(av, chave);
    sem.append(av);
    // Só se escreve alguma coisa quando ainda NÃO há ligação. "Só áudio" seria
    // ruído: a foto sozinha já diz que não há vídeo.
    if (!stream) sem.append(elemento('span', null, 'a ligar…'));
    t.append(sem);
  }

  t.append(elemento('span', 'tile__nome', nomeDoPeer(chave)));
  t.append(accoesDoPainel(chave, { transmite, aVer, temVideo }));
  return t;
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

/** Mantém um <audio> por peer, fora da grelha e imune a redesenhos. */
function sincronizarAudios() {
  const caixa = $('#audios');
  if (!caixa) return;
  const vivos = new Set();
  for (const [peer, l] of voz.pcs) {
    if (!l.stream || !l.stream.getAudioTracks().length) continue;
    vivos.add(peer);
    let el = caixa.querySelector(`audio[data-chave="${peer}"]`);
    if (!el) {
      el = document.createElement('audio');
      el.dataset.chave = peer;
      el.autoplay = true;
      caixa.append(el);
    }
    if (el.srcObject !== l.stream) el.srcObject = l.stream;
    el.muted = surdo || voz.silenciados.has(peer);
  }
  caixa.querySelectorAll('audio').forEach(el => {
    if (!vivos.has(el.dataset.chave)) { el.srcObject = null; el.remove(); }
  });
}

/** Os botões que aparecem quando o rato passa por cima de um painel.
 *
 *  Só mostram o que faz sentido para aquela pessoa naquele momento: não há botão de
 *  silenciar no próprio painel, nem de assistir a quem não está a transmitir.
 */
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
      sincronizarAudios();
      desenharVoz();
    }, mudo));
  }

  return barra;
}

function desenharVoz() {
  const s = servidor();
  const canal = s && s.canais.find(c => c.id === canalAtual);
  const eDeVoz = canal && canal.tipo === 'voz';
  $('#vista-voz').hidden = !eDeVoz;
  desenharNaChamada();
  sincronizarAudios();
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

  // A ver a transmissão de alguém: o ecrã dessa pessoa ocupa tudo e as fotinhas saem.
  if (voz.aVer) {
    const barra = elemento('div', 'assistindo');
    const voltar = elemento('button', 'btn', '← Voltar à sala');
    voltar.onclick = pararDeAssistir;
    barra.append(voltar);
    barra.append(elemento('span', 'assistindo__quem',
      voz.aVer === voz.eu ? 'a ver o teu próprio ecrã' : `a ver ${nomeDoPeer(voz.aVer)}`));
    grelha.append(barra);
    grelha.append(painelDeVoz(voz.aVer, fluxoDe(voz.aVer), { aVer: true }));
    $('#voz-nota').textContent = '';
    return;
  }

  ajustarGrelha(outros.length + 1);
  grelha.append(painelDeVoz(voz.eu, fluxoDe(voz.eu)));
  for (const p of outros) grelha.append(painelDeVoz(p, fluxoDe(p)));

  const ice = servidoresDeGelo().length;
  $('#voz-nota').textContent = ice
    ? `${ice} servidor(es) de ligação configurado(s).`
    : 'Sem servidores de ligação, isto só liga entre máquinas na mesma rede local. ' +
      'Botão direito → Servidores de ligação para configurar um TURN.';
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

listen('presenca', ev => {
  const { peer, canal } = ev.payload;
  if (canal) voz.presentes.set(peer, canal); else voz.presentes.delete(peer);
  if (voz.canal && canal === voz.canal) garantirLigacao(peer);
  if (!canal || canal !== voz.canal) {
    if (voz.aSerVistoPor.delete(peer)) actualizarEspectadores();
    fecharFluxoRecebido(peer);
  }
  if (voz.canal && !canal && voz.pcs.has(peer)) {
    voz.pcs.get(peer).pc.close();
    voz.pcs.delete(peer);
  }
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

/* --- o que tens aberto ----------------------------------------------------- */

async function verJogo() {
  try {
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
  alternarEcra();
};

setInterval(verJogo, 5000);

/* --- ligação de voz --------------------------------------------------------- */

/** Lê a qualidade da ligação das estatísticas do WebRTC, em vez de a inventar. */
async function qualidadeDaLigacao() {
  const ligacoes = [...voz.pcs.values()];
  if (!ligacoes.length) return { ok: true, texto: 'Voz conectada' };
  let pior = 0;
  let algumaLigada = false;
  for (const l of ligacoes) {
    if (l.pc.connectionState === 'connected') algumaLigada = true;
    try {
      const stats = await l.pc.getStats();
      stats.forEach(r => {
        if (r.type === 'candidate-pair' && r.state === 'succeeded' && r.currentRoundTripTime) {
          pior = Math.max(pior, r.currentRoundTripTime * 1000);
        }
      });
    } catch (e) { /* sem estatísticas ainda */ }
  }
  if (!algumaLigada) return { ok: false, texto: 'A ligar…' };
  if (!pior) return { ok: true, texto: 'Voz conectada' };
  return { ok: pior < 250, texto: `Voz conectada · ${Math.round(pior)} ms` };
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
    $('#ligacao-estado').classList.toggle('is-fraco', !q.ok);
    $('#ligacao-sinal').classList.toggle('is-fraco', !q.ok);

    $('#btn-partilhar').classList.toggle('is-on', !!voz.ecra);
    $('#btn-camara').classList.toggle('is-on', !!voz.camara);
    $('#btn-ruido').classList.toggle('is-cortado', !ruidoSuprimido);
    $('#btn-ruido').title = ruidoSuprimido
      ? 'Supressão de ruído ligada'
      : 'Supressão de ruído desligada';
  }

  const t = voz.micro ? voz.micro.getAudioTracks()[0] : null;
  $('#btn-mic').classList.toggle('is-cortado', !!t && !t.enabled);
  $('#btn-mic').title = !t ? 'Sem microfone' : (t.enabled ? 'Silenciar microfone' : 'Ligar microfone');
  $('#btn-surdo').classList.toggle('is-cortado', surdo);
  $('#btn-surdo').title = surdo ? 'Voltar a ouvir' : 'Silenciar tudo';
}

/* --- botões ---------------------------------------------------------------- */

let surdo = false;
let ruidoSuprimido = true;

$('#btn-mic').onclick = () => {
  const t = voz.micro ? voz.micro.getAudioTracks()[0] : null;
  if (t) { t.enabled = !t.enabled; desenharVoz(); desenharRodape(); }
};

$('#btn-surdo').onclick = () => {
  // Ficar surdo silencia tudo o que entra E o próprio microfone, como no Discord:
  // não faz sentido continuar a falar para quem não se consegue ouvir a responder.
  surdo = !surdo;
  document.querySelectorAll('#audios audio').forEach(el => { el.muted = surdo; });
  document.querySelectorAll('#voz-grelha video').forEach(el => {
    if (!el.dataset.proprio) el.muted = surdo;
  });
  const t = voz.micro ? voz.micro.getAudioTracks()[0] : null;
  if (t && surdo) t.enabled = false;
  desenharVoz();
  desenharRodape();
};

$('#btn-ruido').onclick = async () => {
  ruidoSuprimido = !ruidoSuprimido;
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
  desenharRodape();
};

$('#btn-desligar').onclick = () => sairDeVoz();
$('#btn-partilhar').onclick = () => alternarEcra();

$('#btn-camara').onclick = async () => {
  if (voz.camara) {
    voz.camara.getTracks().forEach(t => t.stop());
    voz.camara = null;
    for (const [, l] of voz.pcs) {
      l.pc.getSenders()
        .filter(s => s.track && s.track.kind === 'video' && s.track.label.indexOf('screen') < 0)
        .forEach(s => l.pc.removeTrack(s));
    }
  } else {
    try {
      voz.camara = await navigator.mediaDevices.getUserMedia({ video: { width: 1280, height: 720 } });
    } catch (e) {
      console.warn('sem câmara:', e);
      return;
    }
    for (const [, l] of voz.pcs) {
      voz.camara.getTracks().forEach(t => l.pc.addTrack(t, voz.camara));
    }
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

    const estava = voz.falando.has(chave);
    const agora = proprioSilenciado ? false : (estava ? rms > LIMIAR_SAI : rms > LIMIAR_ENTRA);
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

/** Diz a toda a gente na sala o que estou a enviar. O WebRTC não distingue um ecrã
 *  de uma câmara do outro lado, portanto quem envia é que tem de contar. */
function anunciarEstado() {
  for (const peer of voz.pcs.keys()) {
    sinalizar(peer, { tipo: 'estado', ecra: !!voz.ecra, camara: !!voz.camara });
  }
}

/* --- assistir a uma transmissão -------------------------------------------- */

function assistir(peer) {
  if (voz.aVer && voz.aVer !== peer) sinalizar(voz.aVer, { tipo: 'assistir', ligado: false });
  voz.aVer = peer;
  // Quem transmite tem de saber que estou a ver: e essa lista que decide o que sai da
  // maquina dele. Sem isto o ecra era codificado para ninguem.
  if (peer !== voz.eu) sinalizar(peer, { tipo: 'assistir', ligado: true });
  desenharVoz();
}

function pararDeAssistir() {
  if (voz.aVer && voz.aVer !== voz.eu) sinalizar(voz.aVer, { tipo: 'assistir', ligado: false });
  voz.aVer = null;
  desenharVoz();
}

/* ---------- arranque ---------- */


(async () => {
  voz.eu = await invoke('meu_endereco').catch(() => null);
  await desenharTudo();
  if (!vista.nome) {
    abrir('veu-bemvindo');
    $('#in-nome').focus();
  }
  procurarAtualizacao();
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
})();

/* ---------- autoteste da partilha de ecrã ---------------------------------- */

/* Corre só com `bruma --autoteste`. Parte do princípio de que nada funciona e vai
   verificando: os pedaços chegam? o vídeo aceita-os? tem dimensões? o tempo anda?
   Cada uma dessas perguntas falha de maneira diferente e em sítios diferentes. */
(async () => {
  if (!window.__TAURI__) return;
  if (!(await invoke('autoteste_pedido').catch(() => false))) return;

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
      { servidor: 'autoteste', canalVoz: 'autoteste', saida: canal });
    diz(`autoteste: a captar a ${r.largura}x${r.altura}`);
  } catch (e) {
    return diz(`autoteste FALHOU a arrancar: ${e}`);
  }

  await new Promise(r => setTimeout(r, 6000));
  await invoke('parar_de_partilhar').catch(() => {});

  const v = fluxo.el;
  const intervalos = [];
  for (let i = 0; i < v.buffered.length; i++) {
    intervalos.push(`${v.buffered.start(i).toFixed(2)}-${v.buffered.end(i).toFixed(2)}`);
  }
  diz(`autoteste: ${pedacos} pedaços, ${(bytes / 1e6).toFixed(1)} MB`
    + ` | vídeo ${v.videoWidth}x${v.videoHeight}, readyState=${v.readyState}`
    + ` | descodificados=${v.getVideoPlaybackQuality ? v.getVideoPlaybackQuality().totalVideoFrames : '?'}`
    + ` | bufferizado=[${intervalos.join(', ')}] t=${v.currentTime.toFixed(2)}`
    + ` | erro=${v.error ? v.error.code : 'nenhum'}`
    + (v.error && v.error.message ? ` "${v.error.message}"` : ''));
  fluxo.fechar();

  /* E a segunda prova, por fora do MSE: os mesmos bytes num <video> comum. Se este toca
     e o de cima não, o ficheiro está bom e o problema é do dialeto que o MSE exige; se
     nenhum toca, o problema está antes, no que estamos a produzir. Sem separar as duas
     coisas, o passo seguinte é adivinhar. */
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
