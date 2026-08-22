; Ganchos do instalador do Bruma.
;
; O instalador passou a ser por máquina (Arquivos de Programas). Quem instalou uma versão
; antiga tem o Bruma por utilizador, em %LOCALAPPDATA%\Bruma — e o template do Tauri não
; o vê: ele procura instalações anteriores no MESMO contexto (HKLM para este instalador),
; e a antiga está em HKCU. Sem isto, a pessoa ficava com duas cópias e o atalho antigo a
; abrir a versão velha.
;
; CAUTELA, porque isto corre elevado: nunca se executa o que quer que o registo diga.
; Um UninstallString em HKCU é escrevível sem privilégios — executá-lo às cegas dentro de
; um instalador com direitos de administrador seria oferecer elevação a qualquer coisa
; que lá tivesse sido posta. Só se aceita o caminho EXATO onde as versões antigas do
; Bruma sempre se instalaram, e mais nenhum. Se a pessoa tiver instalado noutra pasta,
; não se adivinha: fica um aviso e ela desinstala à mão.

!macro NSIS_HOOK_PREINSTALL
  ; Há um Bruma por utilizador?
  ReadRegStr $R0 HKCU "${UNINSTKEY}" "UninstallString"
  ${If} $R0 != ""
    StrCpy $R1 "$LOCALAPPDATA\${PRODUCTNAME}"
    ${If} ${FileExists} "$R1\uninstall.exe"
      DetailPrint "A remover a instalação antiga (por utilizador)…"
      ; O _?= faz o desinstalador correr no sítio em vez de se copiar para o temp —
      ; sem ele, o ExecWait voltava logo e apagávamos o chão antes de ele acabar.
      ExecWait '"$R1\uninstall.exe" /S _?=$R1'
      Delete "$R1\uninstall.exe"
      RMDir "$R1"
      DeleteRegKey HKCU "${UNINSTKEY}"
    ${Else}
      MessageBox MB_OK|MB_ICONINFORMATION "Existe uma instalação antiga do ${PRODUCTNAME} noutra pasta. Desinstala-a pelo Painel de Controlo quando puderes — esta instalação segue na mesma."
    ${EndIf}
  ${EndIf}
!macroend
