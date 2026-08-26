# Bruma

Um Discord P2P, cifrado ponta a ponta — **sem servidor**.

Sem e-mail, sem telefone, sem password. A tua identidade é um par de chaves gerado no teu
PC, e **24 palavras recuperam-na** noutra máquina. Não há máquina central a guardar as tuas
mensagens, nem sequer cifradas: quem tem o histórico é quem está online.

> **Estado: v0.16.3 — texto, voz, partilha de ecrã com som, câmara, conversas privadas,
> uma lista de amigos, permissões de mensagens e por-ler com avisos.**
>
> Tudo isso vai pelo mesmo caminho (o iroh) e **não há nada para configurar**. Até à v0.5.2
> a voz precisava de um servidor STUN ou TURN colado à mão nas duas máquinas; deixou de
> precisar. Se o router não se deixar furar, a ligação passa por um relay em vez de falhar —
> mais lenta, e a barra da chamada di-lo.
>
> É software novo, escrito para duas pessoas o usarem. Funciona, mas não teve os anos de
> uso que apanham os casos raros.

## Instalar

Descarrega o instalador da [última versão](https://github.com/Rakjsu/bruma/releases/latest).
O ficheiro é o **`Instalar-Bruma.exe`** — o mesmo que instala à mão e que a app usa para se
actualizar.

O Windows vai avisar que o editor é desconhecido: o instalador não tem assinatura de código
comercial. *Mais informações* → *Executar mesmo assim*. A assinatura que existe garante que
uma **actualização** veio mesmo daqui, e é verificada antes de instalar seja o que for.

A app avisa sozinha quando há versão nova, e nunca instala sem perguntar.

**Windows 10 ou 11.** Duas coisas dependem da versão, e as duas degradam com aviso em vez
de falhar:

- **o som da partilha excluir a voz da própria chamada** precisa do Windows 10 2004 ou mais
  recente. Sem isso, quem estiver a ouvir-te ouve-se a si próprio, e a app di-lo no botão;
- **travar o ritmo da captura na origem** (poupa a placa gráfica) precisa do Windows 11
  24H2. Sem isso a partilha funciona à mesma; só gasta um pouco mais.

Em Windows 10, a moldura amarela à volta do que se partilha também não se consegue tirar.

## Porquê

O Discord funciona muito bem e não se pretende copiá-lo por desporto. O que muda aqui é o
modelo de confiança: ninguém precisa de dar dados para entrar, e não existe um sítio onde o
histórico de toda a gente fique acumulado à espera de ser pedido, vendido ou roubado.

## Como funciona

- **Rede**: [`iroh`](https://github.com/n0-computer/iroh) — QUIC peer-to-peer onde se marca
  por chave pública, não por IP. Hole-punch directo quando dá, relay quando não dá.
- **Identidade**: uma semente de 32 bytes dá a chave Ed25519 que é ao mesmo tempo o teu ID e
  o teu endereço de rede. As mesmas 32 bytes escrevem-se em 24 palavras (BIP39) — clica na
  marca de identidade, no canto superior esquerdo, para as ver.
- **Mensagens**: log append-only, assinado e encadeado por hash, com relógio lógico híbrido
  para não depender de os relógios das máquinas estarem certos. O conteúdo é opaco.
- **Cifra**: XChaCha20-Poly1305 com nonce por mensagem; assinatura Ed25519 sobre o BLAKE3 de
  cada entrada, verificada ao receber **e** ao ler do disco.
- **Voz**: Opus codificado na interface, em datagramas pelo iroh — um pacote perdido não
  atrasa os seguintes.
- **Ecrã**: captado e codificado em H.264 pelo Windows (Media Foundation + a placa gráfica),
  com o som do sistema em AAC na mesma faixa, e traduzido para o dialecto que o navegador
  aceita. Só se envia a quem carregou em «Assistir».
- **Câmara**: H.264 pela interface, várias ao mesmo tempo, pelo mesmo transporte do ecrã.
- **Conversas privadas**: **não há convite**. O identificador da conversa sai das duas chaves
  públicas (BLAKE3 sobre as duas, ordenadas) e a chave sai de um Diffie-Hellman x25519 entre
  elas — os dois lados chegam ao mesmo sozinhos, sem trocar uma palavra sobre isso. Como não há
  segredo a transportar, também não há nada que se possa reencaminhar a um terceiro.

## O que o Bruma **não** faz

Isto está aqui à frente de propósito, porque prometer demais em privacidade é como estes
projectos morrem.

- **Se perderes as 24 palavras E a pasta de dados, a identidade acaba.** Não há conta, não
  há e-mail de recuperação, não há ninguém a quem pedir. Guarda as palavras primeiro.
- **O convite é um segredo, e é eterno.** Ele carrega a chave que decifra o servidor.
  Trata-o como uma password: quem o tiver entra e lê o histórico todo. **Não expira e não se
  revoga** — e por isso não há forma de expulsar ninguém.
- **Aceitar um convite não dá direitos a quem to deu.** Um convite não é assinado: quem o
  escreve escolhe o que lá está, incluindo a chave que diz ser a do anfitrião. Essa chave
  serve para te ligares e trocares o histórico daquela sala, e mais nada — só passa a contar
  como membro depois de escrever lá uma entrada que **decifra**, o que exige a chave da sala.
- **Não há cargos nem permissões.** Qualquer membro pode criar e apagar canais.
- **A chave do servidor nunca roda.** Sem isso não há *forward secrecy*: quem obtiver a
  chave lê o passado e o futuro.
- **A cifra do disco não te protege de quem tem a tua identidade.** O `indice.json` está
  cifrado com uma chave derivada da tua semente — protege uma cópia de segurança que saia de
  casa, não alguém sentado à tua máquina com a `identidade.key` à mão.
- **Numa chamada, quem está do outro lado pode ver o teu IP** quando a ligação é directa —
  que é o caso normal e o desejável, porque é o mais rápido. Por relay não vê.
- **O relay vê metadados**: que chaves falam entre si, quando e quanto. Nunca vê conteúdo.
- **Se ninguém do canal estiver online, não há sincronização.** É o preço de não haver
  servidor. **Nas conversas privadas isto é mais apertado**: são duas pessoas, portanto uma
  mensagem só chega quando os dois estiverem online ao mesmo tempo. Ela fica à espera na
  máquina de quem escreveu, e vai quando puder — nada passa por terceiros, nem cifrado.
- **Uma conversa privada também não tem *forward secrecy*.** A chave sai de um Diffie-Hellman
  entre duas chaves fixas: quem obtiver a tua semente lê o passado todo. É a mesma limitação da
  chave do servidor, e vale a pena repeti-la aqui porque é numa conversa privada que se espera
  o contrário.
- **A lista de amigos é uma decisão tua, e não um acordo.** Ter alguém na lista quer dizer
  que estás disposto a ligar-te a ele — e uma ligação directa mostra-lhe o teu IP. Alguém
  pôr-te na lista dele não lhe dá nada: essa lista não é tua e tu nem a vês.
- **A lista vive nesta máquina, e só aqui.** As 24 palavras recuperam a identidade, não os
  amigos. Sem servidor, não há de onde os trazer de volta — guarda-a com o resto da pasta.
- **O bloqueio é local, e é bom saber o que isso quer dizer.** Bloquear alguém faz o Bruma
  recusar tudo o que vier dele e fechar a ligação que estiver aberta na altura. Não o impede
  de tentar — não há servidor no meio para o impedir por ti. Em compensação, ele não
  distingue estar bloqueado de tu estares desligado: a ligação fecha-se sem uma palavra.
- **Podes escolher quem te pode abrir uma conversa:** toda a gente que tenha a tua chave, só
  quem partilha uma sala contigo (mais os amigos), ou só os amigos. «Partilha uma sala»
  aqui é exacto, e não uma lista que alguém mantém: prova-se com uma entrada que **decifra**
  com a chave dessa sala, coisa que só quem recebeu o convite consegue. Assinar uma entrada
  não chega — qualquer pessoa assina o que quiser com a sua própria chave.
- **Isto decide quem pode COMEÇAR.** Quem já tem uma conversa aberta continua a poder
  escrever nela; fechar essa porta é bloquear.
- **Não há filtros de conteúdo nem de spam.** Não há servidor a analisar nada, e nem sequer
  há imagens. O que os substitui é a definição acima.
- **A chave de um amigo pode ser marcada como verificada**, depois de a comparares com ele
  por outro caminho. Enquanto não estiver, sabes que falas com quem tem aquela chave; não
  sabes se aquela chave é de quem julgas. Numa app sem directório, é isto que substitui «o
  servidor garante que este é o João».
- **Não é anonimato de rede.** Para isso precisas de VPN ou Tor por baixo.
- **O aviso do sistema não leva o texto da mensagem**, a não ser que o ligues. Um aviso do
  Windows não é a app: aparece no ecrã bloqueado, fica no histórico de notificações e é lido
  por quem passar ao pé do computador. Por omissão diz quem e onde, nunca o quê.
- **Um relógio muito adiantado esconde as mensagens da contagem.** O carimbo de hora de uma
  mensagem é escolhido por quem a escreve, e não há aqui relógio comum. Uma mensagem que diga
  vir de mais de um dia no futuro aparece no canal mas não acende a bolha — sem isso, uma só
  mensagem com uma data absurda marcava o canal como lido para sempre.
- **Não há avisos com a app fechada.** Não há servidor a receber por ti: se o Bruma não está
  a correr, a mensagem espera na máquina de quem a escreveu.
- **Não há anexos, imagens, editar, apagar, reacções, respostas nem markdown.** Só texto
  simples, uma linha de cada vez.

## Espreitar por dentro

A app deixa rasto em `%APPDATA%\Bruma\bruma.log` — é por aí que se começa quando alguma
coisa corre mal. Há também bandeiras de diagnóstico:

```bash
bruma --ouvir=5      # que som sai das colunas, e se a captura exclui a própria app
bruma --quem-toca    # que processos estão a produzir som agora
bruma --fontes       # o que o seletor de partilha mostraria, com as miniaturas
bruma --que-jogo     # o que o detector de jogos vê neste momento
```

## Spikes

O projecto começou por responder às perguntas que podiam matá-lo, antes de escrever produto.
O código vive em [`spikes/`](spikes/) e as respostas em [`docs/PLANO.md`](docs/PLANO.md).

| Spike | Pergunta | Resposta |
|---|---|---|
| [1 · rede](spikes/spike1-net/) | Dois PCs em casas diferentes falam sem servidor? | é a base de tudo o que existe hoje — mas **entre duas casas a sério ainda não foi provado** |
| [2 · ecrã](spikes/spike2-screen/) | Dá para partilhar ecrã dentro do Tauri, e a que custo? | dá, mas o navegador desenha uma barra que não sai e codifica por software — por isso a captura passou a ser nativa |
| [3 · fantasma](spikes/spike3-ghost/) | Dá para sincronizar chat por `.onion` sem tor externo? | **bloqueado** — o arranque do Tor não conclui. Não existe na app |

O [teste entre duas casas](docs/TESTE-COM-AMIGO.md) é o que decide o projecto, e duas
máquinas na mesma casa não o substituem.

## Licença

AGPL-3.0-or-later.

Bruma não tem qualquer ligação ao Discord Inc. Não usa o nome, o logo, os sons nem os
emojis deles.
