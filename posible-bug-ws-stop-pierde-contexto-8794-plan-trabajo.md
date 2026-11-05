# Plan de trabajo: issue 8794, Stop en el dashboard pierde el trabajo del turno

Estado: BORRADOR no validado en frío (Fase 7 pendiente). Análisis en `posible-bug-ws-stop-pierde-contexto-8794.md`. Leerlo primero.

## 6.1 Resumen y severidad

- Problema: al detener un turno, el trabajo ya hecho (rondas de herramientas) no queda en el contexto que ve el siguiente turno en varios caminos de salida por cancelación.
- Severidad: priority:p1, S1 según el issue. Candidato `risk:high` (el issue ya lo trae; toca gateway y runtime del bucle de herramientas).
- Lane de PR: dividir en dos.
  - PR 1 (lane B, narrow bug-fix): huecos en memoria H2 y H3-herramientas. Solo `zeroclaw-runtime`.
  - PR 2 (lane D, elevado a arquitectura): persistencia durable de rondas (H1). Requiere decisión de mantenedores antes de codificar.
- Security Advisory privado: no aplica (sin explotación identificada).

## 6.2 Causa raíz confirmada

Ver tabla de evidencia E1 a E12 del análisis. Resumen VERIFICADO:

- `durable_chat_messages` descarta `AssistantToolCalls` y `ToolResults` (`crates/zeroclaw-api/src/model_provider.rs`, ancla `pub fn durable_chat_messages`).
- La rama `was_cancelled` de `ws.rs` persiste solo esa proyección más un marcador.
- `prepare_tool_calls(...).await?` en `turn/mod.rs` sale sin registrar la ronda si la aprobación se cancela.
- `stream_consume.rs` conserva solo `forwarded_text` al cancelar.

## 6.3 Ancla de commit base y drift

- Base: `65f8eb3c9284e396d9de7872c3df6dba993b7093`, fecha 2026-11-05T00:53:40+00:00. Descargado el 2026-11-05 y comprobado con `git ls-remote https://github.com/zeroclaw-labs/zeroclaw refs/heads/master`. Default branch: `master`.
- Re-ejecución con `master` avanzado:
  1. `git fetch origin master && git diff 65f8eb3c..origin/master -- crates/zeroclaw-gateway/src/ws.rs crates/zeroclaw-runtime/src/agent/turn/mod.rs crates/zeroclaw-runtime/src/agent/turn/call_prep.rs crates/zeroclaw-runtime/src/agent/turn/stream_consume.rs crates/zeroclaw-runtime/src/agent/agent.rs crates/zeroclaw-api/src/model_provider.rs`
  2. Si un fragmento ya no está por línea, buscarlo por el texto del ancla de la tabla de evidencia.
  3. Si ya fue corregido, marcar este plan como obsoleto en el encabezado con el commit o PR que lo resolvió, y no proceder.
- Antes de empezar, buscar PRs abiertos: `durable_chat_messages`, `ToolLoopCancelled`, "interrupt", "abort" sobre la ruta WS.

## 6.4 Pasos de corrección

Rama: `fix/ws-stop-keeps-tool-rounds` desde `master` actualizado. No agregar firmas de IA ni coautoría en commits ni PR (regla del proyecto, `AGENTS.md`). Conventional Commits.

Paso 0 (antes de cualquier código): leer en el worktree `.github/ISSUE_TEMPLATE/bug_report.yml`, `.github/pull_request_template.md`, `CONTRIBUTING.md` y `docs/book/src/maintainers/pr-workflow.md`. No se leyeron en el análisis.

PR 1

1. Cancelación en aprobación (H2).
   - Archivo: `crates/zeroclaw-runtime/src/agent/turn/mod.rs`, alrededor de `} = prepare_tool_calls(` (línea 3358 en la base).
   - Cambio: capturar el `Err` de `prepare_tool_calls`; si `is_tool_loop_cancelled(&e)`, construir resultados con la cadena `turn-tool-interrupted-before-result` (ya usada en `mod.rs` línea 3589 y 3615 para el mismo fin) para cada llamada del lote, llamar `turn_state.append_tool_round(...)` con `assistant_history_content` y propagar el error. Reutilizar el bloque de líneas 3570 a 3659 en lugar de duplicarlo: extraer función si hace falta.
   - Validar: `cargo test -p zeroclaw-runtime <nombre_test_nuevo>`.
2. Cancelación durante streaming con tool calls emitidos (H3, parte herramientas). DECISIÓN ABIERTA: no implementar sin revisar con el mantenedor si un tool call incompleto debe reponerse. Evaluar primero si `outcome.tool_calls` está completo en el momento de la cancelación (`stream_consume.rs`, ancla `if forwarded_text.is_empty() {`). Si los argumentos pueden estar truncados, no persistir. Documentar la decisión en el PR.
3. Comandos del PR 1: `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --locked -p zeroclaw-runtime`, `./scripts/ci/rust_quality_gate.sh`.

PR 2 (solo tras decisión de diseño; no codificar a ciegas)

4. Proponer en el issue o en un RFC corto el formato durable: opciones (a) conservar la proyección y agregar un registro estructurado de rondas en el turno interrumpido, siguiendo lo que hizo PR 9378 para ACP; (b) persistir un resumen textual del trabajo hecho junto al marcador. Elegir con mantenedores. Revisar el diff de PR 9378 y de PR 10197 primero.
5. Implementar en `ws.rs` (rama `was_cancelled`, ancla `let needs_marker = match &result {`) y en la restauración (`restore_agent_history`). Pasar por la depuración de secretos existente antes de guardar resultados de herramientas.
6. Validar: `cargo test --locked -p zeroclaw-gateway`, `./dev/ci.sh all` si el entorno lo permite.

## 6.5 Casos de prueba (escribir predicción del rojo antes de ejecutar)

| # | Cubre | Antes del fix | Después |
|---|---|---|---|
| T1 | Cancelar durante aprobación pendiente (H2) con historial previo | `agent.history()` no contiene `AssistantToolCalls` de la ronda cancelada; se espera que la aserción "history contiene la ronda" falle | contiene la ronda con resultado `turn-tool-interrupted-before-result` |
| T2 | Cancelar a mitad de lote con una herramienta completada | ya pasa (E7); sirve de control | sin cambios |
| T3 | Stop vía `/api/sessions/{id}/abort` durante varias rondas, luego un segundo mensaje en la misma conexión (patrón de los tests de `ws.rs` cerca de la línea 3750) | verificar si el segundo prompt enviado al proveedor contiene las rondas previas | debe contenerlas |
| T4 | Mismo escenario con nueva conexión (restauración desde la tienda) | las rondas no están (E3), se espera fallo | según formato de PR 2 |
| T5 | Cancelar con solo thinking streameado | se persiste solo el marcador (E11, INFERIDO) | definir con mantenedores |

Fixtures: el proveedor mock debe emitir herramientas con resultados distintos del texto final para que el test discrimine. Ninguno de estos tests se ha escrito ni ejecutado.

## 6.6 Impacto y validación manual

- Funcional: el siguiente mensaje tras Stop conserva el contexto del trabajo hecho.
- Compatibilidad: PR 1 sin cambio de formato. PR 2 puede cambiar el esquema de transcripción durable: documentar migración y rollback.
- Validación manual A/B: en `master`, iniciar daemon, pedir una tarea con muchas herramientas, detener a mitad y enviar "continúa donde quedaste"; repetir en la rama; comparar si el modelo conoce los resultados previos. Repetir tras recargar la página. Pegar `zeroclaw --version` y salida de `zeroclaw doctor` en el PR (no ejecutados en este análisis).

## 6.7 Estado en GitHub

- Issue 8794: OPEN, existente. No comentar sin que el usuario lo pida. Información útil para agregar si el usuario lo autoriza: tabla E1 a E12, la división en PR 1 y PR 2, y la pregunta de diseño sobre razonamiento no firmado.
- Relacionados: PR 9378 (MERGED, ACP), issue 10121 (CLOSED vía PR 10197), issue 7759 (OPEN), PR 9002 (estado a reconfirmar).

## 6.8 Cruces y coordinación

- PR 1 y PR 2 tocan `ws.rs` y `turn/mod.rs`: NO ejecutar en paralelo con otros PRs sobre esos archivos. PR 1 primero; PR 2 después.
- Coordinar con el trabajo de 7759/9002 (misma función de cierre de turno en `ws.rs`).

## 6.9 Ejecución en la nube

- Un worktree y una rama por PR. Plantilla de PR completa con evidencia real (comandos y salidas).
- Definition of Ready: plantilla completa, alcance explícito, tests vistos en rojo antes del fix.
- Definition of Done: `CI Required Gate` en verde, labels de riesgo acordes, rollback concreto (revert del commit; PR 2 además requiere plan de datos).
- Si hay regresión tras merge: revertir en `master`, abrir issue con causa raíz, reintroducir solo con tests de regresión.
- Aprobaciones: `risk:high` implica dos aprobaciones Core o la vía acelerada de `pr-workflow.md` (no releída).
