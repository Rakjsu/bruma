# Auditoria da Fase 0 — 21/08/2026

Revisão do que foi construído nos três spikes. Cada achado foi verificado contra o código, e os que
não sobreviveram à verificação estão listados no fim para não voltarem a aparecer.

## Estado das correções

| # | Achado | Estado |
|---|---|---|
| 1.1 | XSS pelo título da janela partilhada | **corrigido** — `mostrarCaptura()` sem `innerHTML`, escape na tabela |
| 1.2 | CI vermelho em Linux | **corrigido** — escopo por plataforma, exclui o Tauri em Linux |
| 1.3 | Fuga de `RTCPeerConnection` e botões presos | **corrigido** — `try/finally` em `medir()` e `correrMedicoes()` |
| 1.4 | Falta o ficheiro LICENSE | **corrigido** — texto canónico da AGPL-3.0 |
| 1.5 | 16 MiB alocados antes do handshake | **corrigido** — limite de 64 KiB até a identidade estar provada, nos dois spikes |
| 2.4 | Animações sem `prefers-reduced-motion` | **corrigido** — media query + névoa pausa em segundo plano |
| 2.1 | Cadeia de hash decorativa | **corrigido** — o `prev` passou a definir a ordem e a expor buracos (`orfas()`) |
| 2.2 | Ordem por relógio de parede | **corrigido** — relógio lógico híbrido, com teste de regressão |
| 2.3 | Log reescrito por inteiro | em aberto — morre na Fase 1 com SQLite |
| 2.5 | Raciocínio só nos READMEs | **corrigido** — quatro explicadores na própria interface |

A correção do XSS foi verificada por teste no browser: a técnica antiga injeta a tag (`antigo_injectou: true`),
a nova não (`novo_injectou: false`) e mantém as cores do painel.

A ordenação tem agora teste de regressão: uma resposta escrita com o relógio cinco segundos atrasado
continua a aparecer **depois** da pergunta. Com a ordenação antiga por `ts_ms`, o carimbo 5000
ordenava-se antes do 10000 — a inversão era aritmética, não hipótese.

Fica em aberto apenas o 2.3 (log reescrito por inteiro), que morre na Fase 1 com SQLite.

---

## O que muda decisões

### A ordem dos spikes está errada agora

O plano diz que o Spike 1 (rede) é o mais importante porque, se o P2P falhar, a arquitetura cai.
Isso era verdade quando o projeto era um exercício. **Deixou de ser.**

Desde 17/08/2026 o Discord suspendeu partilha de ecrã e vídeo no Brasil por ordem da ANPD. O dono
está nos EUA; os amigos estão no Brasil. Ou seja, o Bruma existe para repor uma funcionalidade
concreta que foi retirada há quatro dias — e essa funcionalidade é **a partilha de ecrã**, que é
precisamente o que o Spike 2 mede.

Se o Spike 1 falhar, há saída: usa-se o relay, ou aloja-se um relay próprio. **Se o Spike 2 falhar,
o projeto não tem razão de existir na forma atual.** O gate de maior valor passou a ser o 2.

### O problema urgente já tem solução, e isso é bom saber

O dono já tem o Aurora VPN com servidor no Chile, montado exatamente para os amigos brasileiros
recuperarem vídeo no Discord. Isso significa que **o Bruma não é um remédio de emergência** — a dor
imediata já está tratada.

A consequência é libertadora, não desanimadora: dá para construir o Bruma pelas razões certas
(independência de política de terceiros, privacidade, não voltar a ficar refém de uma decisão
regulatória) e com o tempo que for preciso, em vez de à pressa. Mas convém que essa expectativa
esteja explícita, para ninguém achar que isto resolve alguma coisa este mês.

### O relay não tem de ser o público

O plano escolheu o relay público do n0 para não haver infraestrutura. Mas o dono **já tem VPS no
Brasil e no Chile**. O `iroh-relay` é o mesmo crate e aloja-se num comando.

Um relay em Santiago fica geograficamente entre os EUA e o Brasil, e resolve de uma vez três coisas
que o plano lista como limitações: a falta de garantias do relay público, a exposição de metadados a
terceiros, e a latência de encaminhamento. Isto passou a ser a recomendação, não a escotilha de fuga.

---

## 1 · Bugs

Por ordem de retorno sobre esforço.

### 1.1 XSS através do título da janela partilhada
`spikes/spike2-screen/ui/app.js:190` (e `:162`) · **alto**

O relatório de captura é injetado com `innerHTML` depois de passar por `JSON.stringify`. O
`JSON.stringify` **não escapa `<` nem `>`** — só aspas. E `v.label` (linha 179) é o título da janela
ou do ecrã que se está a partilhar, que uma página web controla via `document.title`.

Cadeia de ataque realista: um amigo manda um link, abres a página, a página põe
`document.title = '<img src=x onerror=...>'`, partilhas essa janela → o script corre dentro da app.
Numa app Tauri isso é pior do que num browser, porque a partir daí alcança-se a ponte IPC.

Verificado por teste direto no browser: `injectouTagImg: true`, `jsonEscapaAngulos: false`.

**Correção:** usar `textContent` para o bloco JSON e construir a parte colorida com `createElement`,
ou escapar `<`, `>` e `&` antes de concatenar. O mesmo vale para a tabela em `:307`.

### 1.2 O CI não passa em Linux
`.github/workflows/ci.yml:19` · **alto**

A matriz corre `windows-latest` **e** `ubuntu-latest`, e os passos fazem
`cargo clippy --workspace` e `cargo test --workspace`. O workspace inclui o `spike2-screen`, que
depende do `tauri` — e o Tauri em Linux precisa de `libwebkit2gtk-4.1-dev`, `libgtk-3-dev` e
companhia, que o `ubuntu-latest` não traz instaladas.

Resultado: o CI vai ficar vermelho no primeiro push, por uma razão que não tem nada a ver com o
código.

**Correção:** ou instalar as dependências de sistema no job de Linux, ou excluir o `spike2-screen`
em Linux com `--workspace --exclude spike2-screen`. A segunda é mais honesta enquanto o alvo for
só Windows.

### 1.3 Fuga de `RTCPeerConnection` e botões que ficam presos
`spikes/spike2-screen/ui/app.js:242` e `:296` · **médio**

Nem `medir()` nem `correrMedicoes()` têm `try/finally`.

Em `medir()`, se qualquer `await` entre a criação das duas `RTCPeerConnection` e o `close()` lançar
— negociação SDP falhada, `setCodecPreferences` a recusar um codec — as duas ligações ficam abertas
e a track de ecrã presa. Ao fim das oito medições do teste completo, são até dezasseis.

Em `correrMedicoes()`, a mesma exceção deixa os dois botões desativados para sempre. O utilizador
conclui que a app está partida e recarrega, perdendo o relatório.

**Correção:** `try/finally` nos dois — fechar as `pc` no `finally` de `medir()`, reativar os botões
no `finally` de `correrMedicoes()`.

### 1.4 Falta o ficheiro LICENSE
raiz do repositório · **médio**

O `Cargo.toml` declara `license = "AGPL-3.0-or-later"` mas não há ficheiro `LICENSE`. Num projeto
que pode vir a ser público, e que escolheu AGPL de propósito, a licença sem texto não vincula nada.

**Correção:** juntar o texto da AGPL-3.0 em `LICENSE`.

### 1.5 Alocação de 16 MiB antes de o peer estar autenticado
`spikes/spike3-ghost/src/main.rs` (`ler()`) · **baixo, mas propaga-se**

`read_msg`/`ler` faz `vec![0u8; n]` com `n` até `MAX_FRAME` (16 MiB) a partir do prefixo de tamanho,
**antes** de qualquer verificação de identidade. No Spike 1 isso é atenuado porque o iroh já
autenticou o peer por TLS. No Spike 3 não é: qualquer um que conheça o `.onion` pode abrir uma
ligação e mandar um prefixo de 16 MiB repetidamente.

Num spike é irrelevante. **Na Fase 1 não é**, e é o tipo de coisa que se copia sem pensar.

**Correção:** limite muito mais baixo (64 KiB) até o handshake estar concluído, e só depois subir.

---

## 2 · Melhorias que valem o esforço

### 2.1 A cadeia de hash é decorativa
`spikes/spike-common/src/log.rs:19,31,122`

O campo `prev` é preenchido com a cabeça do log e está coberto pela assinatura — mas **nunca é
verificado nem usado**. Não há código que confirme que `prev` aponta para uma entrada existente, nem
que use a cadeia para ordenar.

Os commits e os READMEs chamam-lhe "log assinado encadeado por hash". A parte "assinado" é verdade;
a parte "encadeado" ainda não faz nada. Isso é uma promessa de integridade que o código não cumpre, e
é pior do que não ter o campo, porque dá segurança falsa a quem ler.

**Ou** verificar a cadeia e usá-la, **ou** dizer nos documentos que `prev` é por agora só uma
referência causal guardada para uso futuro.

### 2.2 A ordem das mensagens depende dos relógios
`spikes/spike-common/src/log.rs` (`ordered()`)

A ordenação é `(ts_ms, hash)`, com `ts_ms` vindo do relógio local de cada peer. O plano fala em
"Lamport timestamp com desempate na pubkey" para as operações de admin, mas o log de mensagens usa
relógio de parede.

Entre uma máquina nos EUA e outra no Brasil, alguns segundos de desvio bastam para uma resposta
aparecer **antes** da pergunta. Num chat isso é visível e irritante, e é exatamente o tipo de bug
que se manifesta só em produção porque em testes locais os relógios coincidem.

O `prev` que já existe (ver 2.1) é precisamente a informação de que se precisa para ordenar por
causalidade em vez de por relógio. As duas coisas resolvem-se juntas.

### 2.3 O log é reescrito por inteiro a cada mensagem
`spikes/spike-common/src/log.rs` (`append_local` → `save`)

Cada mensagem serializa e reescreve o ficheiro JSON completo. É O(n²) ao longo de uma conversa, e o
`Log::load` verifica a assinatura de todas as entradas ao arrancar.

Num spike com dezenas de mensagens é invisível. Com dez mil mensagens são dez mil reescritas de um
ficheiro que cresce, mais dez mil verificações Ed25519 no arranque.

O plano já diz que a Fase 1 usa SQLite com SQLCipher, portanto isto morre naturalmente. **Vale a
pena estar escrito no spike** para não ser promovido a produto por distração.

### 2.4 Animações infinitas sem respeitar `prefers-reduced-motion`
`spikes/spike2-screen/ui/app.css:72,227,345`

Três animações em ciclo infinito: a névoa, o anel de quem está a falar, e os pontos de "está a
escrever". Nenhuma respeita `prefers-reduced-motion`, que é a preferência do sistema para quem tem
sensibilidade a movimento.

A da névoa tem um custo adicional: é um `filter: blur(34px)` sobre uma camada fixa de ecrã inteiro,
a animar continuamente. Numa app de chat que fica aberta o dia todo, isso é GPU a trabalhar sem
parar — e bateria, num portátil.

**Correção:** um bloco `@media (prefers-reduced-motion: reduce)` que anula as três, e pausar a névoa
quando a janela perde o foco (`document.visibilityState`).

### 2.5 O raciocínio ainda vive nos READMEs, não na app

A preferência documentada do dono é clara: *"se vale a pena escrever no chat, vale a pena estar na
app"*. O shell já cumpre isso em dois sítios — o aviso do Modo Fantasma diz o que se perde, e a
barra de topo mostra o estado do transporte.

Mas continua a haver coisas importantes que só existem em ficheiros `.md`:

- **porque é que uma chamada em mesh mostra o IP** e o que fazer quanto a isso;
- **porque é que o histórico depende de alguém estar online** — a contrapartida de não haver servidor;
- **o que significa "direto" e "por relay"** na barra de topo, para quem não sabe o que é hole-punch;
- **o que a expulsão de um membro garante e o que não garante** (rotação de chave vs. histórico já
  transferido).

Tudo isto são decisões que o utilizador precisa de entender para confiar na app. Um ícone de
interrogação ao lado de cada estado, com duas linhas de explicação, resolve.

---

## 3 · Recomendações

### 3.1 Alojar o relay no Chile, e não usar o público

Já explicado acima. É a mudança de maior impacto e usa infraestrutura que já existe.

### 3.2 O teste com o amigo vai falhar por razões não técnicas

O `docs/TESTE-COM-AMIGO.md` assume um amigo confortável com a linha de comandos. A realidade
provável, do lado brasileiro:

- um `.exe` **sem assinatura digital**, vindo do estrangeiro, por WhatsApp — o SmartScreen bloqueia e
  o Windows Defender pode apagá-lo em silêncio;
- **abrir uma linha de comandos naquela pasta** não é óbvio para quem nunca o fez;
- **copiar 64 caracteres** por WhatsApp corre mal: a app quebra a linha, ou o amigo copia com espaço.

**Correção barata:** juntar um `.bat` de duplo clique que corre o programa com os argumentos certos e
não fecha a janela no fim, e mudar o `EndpointId` de hex para z-base-32 (o iroh já traz
`EndpointId::from_z32`), que corta o comprimento para cerca de metade.

### 3.3 A latência EUA↔Brasil não está no plano e devia

O plano discute largura de banda em detalhe e **não diz uma palavra sobre latência**. Com ~120-150 ms
de base entre os EUA e o Brasil:

- **para partilha de ecrã** é irrelevante — ninguém nota 150 ms a ver código;
- **para voz** está no limite do aceitável, e a árvore de distribuição da camada 4 (+50-150 ms por
  salto) tornaria a conversa desconfortável;
- portanto **a árvore serve para ecrã, não para voz** — e isso tem de estar escrito, senão alguém
  a aplica aos dois.

### 3.4 O upload assimetrico do cabo merece uma linha explícita

A tabela do orçamento de upload está correta nos números, mas assume implicitamente que há upload
disponível. Uma ligação de cabo residencial nos EUA costuma dar 10-35 Mbps de subida contra centenas
de descida.

Cruzando com a tabela do plano: partilhar **código** para 3-5 pessoas cabe folgadamente. Partilhar
**jogo a 1080p60** para 5 pessoas (15-25 Mbps mesmo com as camadas 1-3) ocupa praticamente todo o
upload disponível — e nessa altura a voz, que partilha o mesmo canal, começa a cortar.

Não invalida nada, mas devia estar dito: **é a partilha de jogo, não a de código, que vai obrigar
ao SFU ou à árvore.**

### 3.5 Exposição legal, que é melhor pensar agora do que depois

O Discord foi bloqueado no Brasil por falhas de verificação de idade e de remoção de conteúdo. O
Bruma é, por desenho, uma app anónima, cifrada ponta a ponta e sem servidor — ou seja, **estruturalmente
incapaz** de fazer qualquer uma dessas duas coisas.

Enquanto for um grupo de amigos, isto é um não-problema. Se algum dia for distribuído publicamente,
é o problema principal, e não se resolve com código: resolve-se com escolhas sobre quem pode entrar
(só por convite assinado, como já está desenhado) e sobre o que o projeto diz de si próprio.

Vale a pena a decisão consciente **antes** de haver utilizadores, não depois.

---

## Achados descartados

Registados para não voltarem:

- **"Não há anel de foco visível para navegação por teclado"** — falso. A medição foi feita com
  `.focus()` programático, e o `:focus-visible` do Chromium só se aplica a foco por teclado. Nenhuma
  regra do CSS remove o `outline` dos botões; o único `outline: 0` está no campo de texto do
  composer, e aí o `:focus-within` do contentor dá indicação visual.
- **"O toggle do Modo Fantasma parte o ponto de estado à segunda passagem"** — falso. Testado com
  quatro cliques seguidos: alterna corretamente entre `dot dot--ok dot--warn` e `dot dot--ok`.
- **"O `.gitignore` não cobre o estado do Tor do spike 3"** — falso. `git check-ignore` confirma que
  `spikes/*/data/` apanha `spikes/spike3-ghost/data/ana-tor/state/keystore`.

---

## O que eu faria a seguir

1. **Correr o Spike 2 e apontar os números.** É o gate que decide o projeto, e está à espera de
   cinco minutos de cliques.
2. **Corrigir o XSS e o CI.** Meia hora, e são os dois únicos achados de severidade alta.
3. **Alojar o `iroh-relay` no VPS do Chile.** Resolve três limitações do plano de uma vez, com
   infraestrutura que já existe.
4. **Preparar o teste com o amigo a sério** — `.bat` de duplo clique e ID em z-base-32 — e só depois
   pedir-lhe o favor. Um primeiro teste falhado por SmartScreen queima a boa vontade.
5. **Decidir sobre o `prev`**: verificar a cadeia e ordenar por causalidade, ou tirar a palavra
   "encadeado" dos documentos. As duas são aceitáveis; a mistura atual não é.
