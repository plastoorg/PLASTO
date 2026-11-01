@echo off
:: =====================================================================
::  PLASTO · сборка
::  Запускать двойным щелчком или из терминала в этой папке.
::  На выходе: target\release\plasto.exe — один файл, ничего не устанавливает.
:: =====================================================================
setlocal
chcp 65001 >nul
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
cd /d "%~dp0"

echo.
echo   PLASTO build
echo   ============
echo.

where cargo >nul 2>&1 || (
    echo [X] Rust не найден в PATH. Установите с https://rustup.rs
    pause & exit /b 1
)

:: Все наборы: половина логики, которая правит системные файлы, лежит в
:: ai/hosts/dns, и пропускать их тесты перед сборкой — значит собирать вслепую.
echo [1/2] Тесты...
cargo test || (
    echo [X] Тесты не прошли — сборка остановлена
    pause & exit /b 1
)

echo.
echo [2/2] Сборка...
cargo build --release || (
    echo [X] Сборка не прошла
    pause & exit /b 1
)

echo.
echo   Готово: target\release\plasto.exe
echo.
pause
