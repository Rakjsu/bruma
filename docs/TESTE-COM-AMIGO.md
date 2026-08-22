# Testar com um amigo, em casas diferentes

Este é o teste que decide o projeto. Tudo no Bruma assume que dois computadores em casas
diferentes se ligam diretamente, sem servidor no meio. **Duas máquinas na mesma casa não
provam nada** — partilham o mesmo router, e é justamente o router que costuma ser o
problema.

Demora uns 20 minutos.

---

## Antes de começar: o que já se sabe que vai e não vai funcionar

Vale a pena saber isto de antemão, para não se perder tempo a investigar o que já está
explicado.

| | Estado | Porquê |
|---|---|---|
| Entrar, criar servidor, convidar | deve funcionar | vai pelo iroh |
| Mensagens, e receber o que se perdeu | deve funcionar | vai pelo iroh |
| **Partilha de ecrã** | deve funcionar | desde a v0.5.0 vai pelo iroh também |
| **Voz** | **não vai ligar** | falta configurar servidores de ligação — ver abaixo |

A voz é a única coisa que ainda depende de WebRTC, e o WebRTC precisa de um servidor que
lhe diga por onde furar o router. Sem isso ele só encontra caminhos dentro da rede local,
e entre duas casas não há nenhum. **Não é uma avaria: é configuração que falta.**

Há um servidor de ligação pronto (coturn, no Oracle do Brasil), mas as portas dele estão
fechadas na firewall da Oracle. Enquanto estiverem, a voz não liga a ninguém de fora.

---

## 1 · Instalar (os dois)

Descarregar o instalador mais recente:

**https://github.com/Rakjsu/bruma/releases/latest**

O ficheiro é `Bruma_x.y.z_x64-setup.exe`.

> **O Windows vai avisar** que é de um editor desconhecido — *"O Windows protegeu o seu
> PC"*. É normal e é esperado: assinar uma aplicação de forma a calar esse aviso exige um
> certificado comercial, que custa algumas centenas de euros por ano. Para instalar:
> **Mais informações → Executar mesmo assim**.
>
> O instalador não é anónimo por ser desconhecido; é desconhecido porque não foi comprado
> um certificado. São coisas diferentes e convém dizê-lo em vez de pedir confiança cega.

**Os dois têm de ter a mesma versão.** O formato das mensagens na rede mudou na v0.5.0
para passar a levar vídeo: uma app v0.4.x e uma v0.5.x não se entendem, e a ligação cai
sem explicação útil.

Na primeira abertura cada um escolhe um nome. **Não há registo, nem e-mail, nem password**
— é criada uma chave neste computador e é ela a identidade.

---

## 2 · Um cria o servidor, o outro entra

**Quem cria** (digamos, tu):

1. Criar um servidor.
2. Carregar em **Convidar** e copiar o código.

**Manda o convite por um sítio privado** — Signal, WhatsApp, o que for.

> ⚠️ **O convite contém a chave do servidor.** Quem o tiver consegue ler tudo o que for
> escrito a partir do momento em que entra. Trata-o como uma password, não como um
> endereço. Não o ponhas num sítio público.

**Quem entra** (o teu amigo): cola o convite.

Deve aparecer o servidor, os canais, e o teu nome na lista de membros. Em cima, o contador
de ligados passa a **1 ligado**.

**Se isto funcionar, a premissa do projeto está provada.** Não há servidor nenhum no meio:
os dois computadores estão a falar diretamente, e o convite só levou a chave.

---

## 3 · Mensagens, e o que acontece quando alguém fecha

1. Escrevam os dois num canal de texto. Devem aparecer dos dois lados em segundos.
2. **O teste que interessa:** o teu amigo fecha o Bruma completamente. Tu escreves três ou
   quatro mensagens. Ele volta a abrir.
3. As mensagens devem aparecer-lhe todas, pela ordem certa.

Isto é o "nada morre por estares offline" — e nota que ninguém as guardou num servidor:
estavam no computador de quem as escreveu, à espera de alguém a quem as dar.

**Se as mensagens aparecerem trocadas**, diz-me: a ordenação usa um relógio lógico
precisamente para aguentar relógios desencontrados entre os EUA e o Brasil, e um erro aí é
exatamente o tipo de coisa que nunca se vê a testar na mesma máquina.

---

## 4 · Partilha de ecrã

Os dois entram no mesmo canal de voz. **A voz não vai funcionar** (ver o quadro em cima),
mas a partilha de ecrã não depende dela.

1. Um carrega em **Partilhar ecrã** (o segundo botão da fila, em baixo).
2. Do outro lado, o painel dessa pessoa deve mudar para *"está a transmitir"* com um botão
   **Assistir**.
3. Carregar em Assistir. O ecrã deve aparecer.

O que se está a provar aqui, e não é pouco: **não aparece nenhuma barra do Windows a dizer
"está a partilhar"**, e o vídeo vai pelo mesmo caminho cifrado das mensagens — quem vê o
teu ecrã não fica com o teu endereço.

Se o painel disser *"está a transmitir"* mas a imagem não chegar, isso é informação útil:
quer dizer que o aviso passou e os dados não. Diz-me e eu sei onde procurar.

---

## 5 · Para a voz funcionar (só tu podes fazer isto)

Na consola da Oracle, na instância do Brasil:

**Networking → Virtual Cloud Network → Security Lists → Default → Add Ingress Rules**

Três regras, todas com origem `0.0.0.0/0`:

| Protocolo | Portas |
|---|---|
| UDP | 3478 |
| TCP | 3478 |
| UDP | 50000–50200 |

Depois, **nos dois computadores**: botão direito na app → *Servidores de ligação* → colar:

```
turn:bruma:uvvqIcnTW19wgvJumCTl0gBpk0OC@168.138.155.90:3478
```

Avisa-me quando abrires e eu confirmo daqui que o servidor responde, antes de valer a pena
tentarem a voz.

---

## O que me dizer no fim

Mesmo que corra tudo bem, há coisas que só se veem aí:

- **Em que passo é que parou**, se parou. Cada passo falha por uma razão diferente.
- **Quanto tempo demorou a ligar** — se demorou mais de uns segundos, o furo no router
  falhou e a ligação foi por um relay, o que se nota na velocidade.
- **Se a partilha de ecrã ficou fluida ou aos solavancos**, e o que estava no ecrã (texto
  parado gasta quase nada; um jogo gasta muito).
- Se apareceu alguma janela de erro, o texto dela tal como está.

---

## Se nada ligar

Antes de concluir o que quer que seja, vale a pena isolar o problema com o `spike1-net`,
que faz só a parte da rede e mais nada — se ele ligar e a app não, o problema não é o
router. Está em `spikes/spike1-net/README.md`.
