; Wiqo's own checks before the files are copied (tauri.conf.json bundle.windows.nsis.installerHooks).
; Tauri's installer runs on every Windows (it is a 32-bit program), so without these an ARM64 build installs on an x64 PC (or an x64 build
; on 32-bit Windows) and then refuses to start with "Bu uygulama bilgisayarınızda çalışamıyor".

!macro NSIS_HOOK_PREINSTALL
  !if "${ARCH}" == "arm64"
    ${IfNot} ${IsNativeARM64}
      MessageBox MB_ICONSTOP|MB_OK "Bu kurulum ARM işlemcili Windows içindir.$\r$\n$\r$\nBu bilgisayar için x64 sürümünü (Wiqo_..._x64-setup.exe) wiqoai.com'dan indir." /SD IDOK
      Abort
    ${EndIf}
  !else if "${ARCH}" == "x64"
    ${IfNot} ${RunningX64}
      MessageBox MB_ICONSTOP|MB_OK "Wiqo 64 bit Windows gerektirir; bu bilgisayarda 32 bit Windows var." /SD IDOK
      Abort
    ${EndIf}
  !endif
!macroend
