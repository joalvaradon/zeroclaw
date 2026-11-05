# Retomar el trabajo del issue 8794 en Windows 11

Contexto: el análisis, el plan y los tests se hicieron en un contenedor de nube, que es efímero. Todo lo que importa está en la rama `fix/8794-ws-stop-keeps-tool-rounds` del fork `joalvaradon/zeroclaw`. El código del upstream (`zeroclaw-labs/zeroclaw`, `master` en `65f8eb3c9284e396d9de7872c3df6dba993b7093`) no se modificó en ningún repositorio remoto: los tests viven en un parche.

Contenido de la rama del fork (solo estos archivos nuevos en la raíz):

- `posible-bug-ws-stop-pierde-contexto-8794.md`: análisis con tabla de evidencia, contrato leído y resultados de ejecución.
- `posible-bug-ws-stop-pierde-contexto-8794-plan-trabajo.md`: plan de corrección (PR 1 y PR 2) y casos de prueba.
- `posible-bug-ws-stop-pierde-contexto-8794-tests-runtime.patch`: 13 tests (T1 a T12 y las dos variantes de T9) listos para aplicar. Modifica `crates/zeroclaw-runtime/src/agent/safety_net.rs` y `crates/zeroclaw-gateway/src/ws.rs`.
- Este archivo.

## 1. Requisitos en Windows 11

1. Git para Windows.
2. Rust con el toolchain MSVC: instalar `rustup` y las Build Tools de Visual Studio con la carga "Desktop development with C++". El proyecto exige rustc 1.96 o superior (`rust-version` del `Cargo.toml`); el análisis usó 1.97.0.
3. Espacio libre: la compilación de pruebas de runtime y gateway ocupó unos 8 GB en Linux. Conviene reservar más de 15 GB.
4. Opcional: Claude Code para Windows, para continuar con asistencia.

## 2. Obtener el upstream y los archivos (PowerShell)

```powershell
# Upstream limpio, en la base del análisis
git clone https://github.com/zeroclaw-labs/zeroclaw.git C:\repos-git\zeroclaw
cd C:\repos-git\zeroclaw
git fetch origin master
git worktree add --detach C:\repos-git\zeroclaw-8794 65f8eb3c9284e396d9de7872c3df6dba993b7093

# Traer los archivos de análisis desde el fork sin mezclar historiales
git remote add fork https://github.com/joalvaradon/zeroclaw.git
git fetch fork fix/8794-ws-stop-keeps-tool-rounds
$b = "fork/fix/8794-ws-stop-keeps-tool-rounds"
$names = "posible-bug-ws-stop-pierde-contexto-8794.md","posible-bug-ws-stop-pierde-contexto-8794-plan-trabajo.md","posible-bug-ws-stop-pierde-contexto-8794-tests-runtime.patch","posible-bug-ws-stop-pierde-contexto-8794-retomar-windows.md"
New-Item -ItemType Directory C:\repos-git\analisis-8794 -Force | Out-Null
foreach ($n in $names) { git show "${b}:$n" | Out-File -Encoding utf8NoBOM "C:\repos-git\analisis-8794\$n" }
```

Si `master` avanzó, antes de seguir compara: `git diff 65f8eb3c9284e396d9de7872c3df6dba993b7093..origin/master -- crates/zeroclaw-gateway/src/ws.rs crates/zeroclaw-runtime/src/agent crates/zeroclaw-api/src/model_provider.rs`. Si cambió algo relevante, sigue el procedimiento de deriva de la sección 6.3 del plan.

El parche se guardó con finales de línea LF. Si Git para Windows convierte a CRLF, aplica con `git apply --ignore-whitespace` o desactiva la conversión: `git config core.autocrlf false` antes de clonar.

## 3. Aplicar el parche y ejecutar los tests

```powershell
cd C:\repos-git\zeroclaw-8794
git switch -c fix/ws-stop-keeps-tool-rounds
git apply --check C:\repos-git\analisis-8794\posible-bug-ws-stop-pierde-contexto-8794-tests-runtime.patch
git apply C:\repos-git\analisis-8794\posible-bug-ws-stop-pierde-contexto-8794-tests-runtime.patch

cargo test -p zeroclaw-runtime --lib --locked -- issue_8794 --test-threads=1
cargo test -p zeroclaw-gateway --lib --locked -- issue_8794 --test-threads=1
```

Resultado esperado en Linux, sobre `65f8eb3c` sin fix:

- runtime: 7 verdes y 1 rojo (T1 `issue_8794_t1_cancel_during_approval_records_the_round`).
- gateway: T3 y T12 verdes; T4, T10 y T11 rojos.

Si en Windows algún resultado difiere, anótalo: los tests de gateway abren sockets en `127.0.0.1` y usan SQLite real. No se ejecutó nada de esto en Windows. Los dos tests de gateway que dependen de temporización (T10, T12) conviene repetirlos varias veces.

## 4. Siguiente trabajo (resumen del plan)

1. PR 1 (`zeroclaw-runtime`): registrar la ronda cuando se cancela durante la aprobación (T1 pasa a verde) y decidir el caso de tool calls emitidos en el stream (T6). Respetar la restricción del plan: no repetir `abandon_unexecuted_prepared_contexts`.
2. Resolver H6 (T11): no avanzar el watermark si el reemplazo durable falla en la rama cancelada de `ws.rs`.
3. PR 2: persistencia durable de rondas de herramientas (T4, T10). Requiere decisión de mantenedores y declaración de privacidad de datos en reposo; ver el plan, pasos 4 y 4 bis.
4. Leer en el worktree, antes de redactar el PR: `.github/pull_request_template.md`, `CONTRIBUTING.md` y `docs/book/src/maintainers/pr-workflow.md`. Sin firmas ni atribución de IA en commits ni PR (regla del proyecto).
5. Validar antes de publicar: `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, `./scripts/ci/rust_quality_gate.sh`, `cargo doc --no-deps --workspace --exclude zeroclaw-desktop` y, para cambios en `zeroclaw-runtime`, `./scripts/ci/parallel_runtime_test_gate.sh`. Los scripts `.sh` requieren Git Bash o WSL.

## 5. Continuar con Claude Code en Windows

La opción más simple es abrir Claude Code en `C:\repos-git\zeroclaw-8794` y pedirle que lea primero el análisis y el plan, que se escribieron para un ejecutor sin contexto previo. Prompt sugerido:

> Lee `C:\repos-git\analisis-8794\posible-bug-ws-stop-pierde-contexto-8794.md` y `...-plan-trabajo.md`. Verifica que `git rev-parse HEAD` sea `65f8eb3c9284e396d9de7872c3df6dba993b7093`, aplica el parche de tests si aún no está y ejecuta el PR 1 del plan, paso a paso. Antes de cada edición vuelve a leer el código por el texto ancla, no por número de línea.

Alternativa: `claude --teleport session_01TPcXVWjBbBBg2gn6uetQiB` desde un clon del fork con el árbol limpio, mientras la sesión de nube siga existiendo. Se escribió de memoria y no se verificó; confirma con `claude --help`. Esa vía no trae la caché de compilación del contenedor.

## 6. Qué no se hizo

- Reproducción en un daemon real.
- Cualquier fix; los cuatro tests rojos siguen rojos.
- Los tests no se ejecutaron en Windows, ni la suite completa de los crates, ni `parallel_runtime_test_gate.sh`.
- PR 2 depende de una decisión de diseño de los mantenedores.
