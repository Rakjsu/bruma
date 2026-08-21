# Teste do Spike 1 — duas casas, duas redes

Este é o teste que decide o projeto. Tudo o resto no Bruma assume que dois computadores em casas
diferentes conseguem falar diretamente, sem servidor. Se isso não se aguentar, a arquitetura muda
toda — e é muito melhor saber isso agora.

**Demora uns 15 minutos** e não precisa de instalar nada do lado do teu amigo.

## O que enviar ao teu amigo

Um único ficheiro:

```
target/release/spike1-net.exe
```

Pesa cerca de 16 MB. Manda por onde quiseres. Ele não precisa de Rust, nem de Node, nem de nada.

> O Windows pode avisar que é de um "editor desconhecido" — é normal para um executável sem
> assinatura digital. Ele tem de escolher *Mais informações → Executar mesmo assim*.

## Preparar (cada um na sua casa)

Cada um põe o `spike1-net.exe` numa pasta vazia própria, por exemplo `C:\bruma-teste\`.
O programa cria lá dentro uma pasta `data/` com a identidade e o histórico.

Abrir uma linha de comandos **nessa pasta** e correr:

**Tu:**
```
spike1-net.exe --name eu
```

Vai imprimir uma linha assim:

```
identidade : 27a76005040e171d314e3c4c1c898ecdffebfc2cbe2b4811beaa71807c6650f3
```

Copia esse código todo e manda ao teu amigo (WhatsApp, Signal, o que for — não é segredo, é o
equivalente a um número de telefone).

**O teu amigo:**
```
spike1-net.exe --name amigo --connect 27a76005040e171d314e3c4c1c898ecdffebfc2cbe2b4811beaa71807c6650f3
```

## O que deve acontecer

Nos dois lados aparece:

```
[ok] Ligado a ...
[ok] Chave de sessao estabelecida (prekey assinada e verificada)
```

A partir daí escrevem os dois e carregam Enter. As mensagens devem aparecer do outro lado.

## O número que interessa: direto ou por relay

Nos primeiros segundos vai aparecer uma destas linhas:

```
[!] caminho inicial: RELAY (hole-punch nao passou -- sinal de CGNAT)
[ok] passou a DIRETO (...) -- hole-punch feito
```

**Esperem uns 30 segundos antes de tirar conclusões.** O hole-punch acontece depois de a ligação
abrir, portanto começar em RELAY é normal. O que interessa é se acaba em DIRETO.

- **Acabou em DIRETO** → a premissa aguenta-se. Podemos seguir sem infraestrutura nenhuma.
- **Ficou em RELAY para sempre** → pelo menos um dos lados está atrás de CGNAT. Continua a
  funcionar, mas todo o tráfego passa por um relay público que não é nosso. Nesse caso a decisão
  passa a ser alojar um relay próprio, e é melhor sabê-lo agora do que daqui a três meses.

## O segundo teste: sobreviver a estar offline

Este prova o "não perco mensagens enquanto durmo", que é a razão de não haver servidor.

1. Com os dois ligados, troquem duas ou três mensagens.
2. **O teu amigo fecha o programa** (Ctrl+C ou fechar a janela).
3. **Tu escreves mais três mensagens.** Ele não está lá — mas escreve na mesma.
4. Ele volta a abrir com exatamente o mesmo comando de antes.
5. **Ele tem de ver as três mensagens que perdeu**, pela ordem certa.

Se isto funcionar, o modelo sem servidor está provado na prática.

## Vale a pena repetir em cenários diferentes

Cada combinação testa uma coisa distinta:

| Cenário | O que testa |
|---|---|
| fibra de casa ↔ fibra de casa | o caso normal |
| fibra ↔ dados móveis (hotspot do telemóvel) | **o pior caso** — o móvel é quase sempre CGNAT |
| um dos lados com VPN ligada | se a VPN estraga o hole-punch |

## O que me interessa saber

Copia e manda:

1. A linha do caminho de cada lado (DIRETO ou RELAY, e ao fim de quanto tempo).
2. Se o teste de offline funcionou.
3. Que tipo de ligação tem cada um (fibra, cabo, móvel) e o operador.
4. Qualquer mensagem de erro que tenha aparecido.

## Se correr mal

**"ligacao falhou"** — normalmente é o ID mal copiado. São 64 caracteres, sem espaços. Confirmem
que não foi cortado pela app de mensagens.

**Fica preso em "relay : a ligar..."** — não há saída para a internet, ou uma firewall corporativa
está a bloquear. Vale a pena tentar com o hotspot do telemóvel.

**O Windows bloqueia o executável** — *Mais informações → Executar mesmo assim*.

**A firewall do Windows pergunta** — permitir em redes privadas chega.

## O que o teste não é

Não há interface, não há voz, não há partilha de ecrã. É de propósito: isto testa uma coisa só, e
o objetivo é uma resposta clara a uma pergunta, não uma demonstração bonita.

E o teu amigo pode apagar a pasta `C:\bruma-teste\` no fim — a identidade dele vive lá dentro e
não deixa nada no computador.
