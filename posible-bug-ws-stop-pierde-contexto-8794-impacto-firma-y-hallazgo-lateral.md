# Impacto: cambiar la firma de `prepare_tool_calls` y corregir el hallazgo lateral

Base: `zeroclaw-labs/zeroclaw` `master` en `65f8eb3c9284e396d9de7872c3df6dba993b7093`, 2026-11-05. Todo lo marcado "medido" se ejecutó en el clon del upstream (Linux, rustc 1.97.0) con los comandos indicados. Parches: `...-fix-pr1.patch` (fix simple), `...-fix-pr1-firma.patch` (fix con cambio de firma, alternativa al anterior) y `...-tests-runtime.patch` (ahora incluye T13 y T14).

## 1. Cambiar la firma de `prepare_tool_calls`

### Qué problema resuelve

El fix simple de PR 1 hace que una cancelación durante la aprobación registre la ronda, pero el error de `prepare_tool_calls` descarta los resultados que la preparación ya había resuelto. Todas las llamadas de la respuesta del modelo quedan como "interrumpidas".

Demostración medida con T13 (`issue_8794_t13_cancel_during_approval_keeps_earlier_denial_result`): lote de dos llamadas, el operador deniega la primera y el turno se cancela mientras la segunda espera aprobación.

| Variante | Resultado de T13 | Lo que queda en `new_messages` para la primera llamada |
|---|---|---|
| Fix simple (sin cambio de firma) | ROJO (medido) | `[interrupted by user before this tool produced a result]` en `t13a`, aunque la interfaz ya mostró la denegación del operador |
| Con cambio de firma | VERDE (medido) | La denegación real ("Denied by user..."); solo la segunda llamada queda como interrumpida |

La diferencia es de fidelidad (método §1, dirección b): el historial que ve el modelo deja de contradecir lo que vio el operador. No hay riesgo de seguridad en ninguna de las dos variantes: el fix simple es más conservador, porque nunca afirma un resultado que no tiene.

### Opciones

| Opción | Descripción | Estado |
|---|---|---|
| Simple | Sin cambio de firma. Todas las llamadas quedan interrumpidas. | Escrita y verificada (`...-fix-pr1.patch`). |
| B | `prepare_tool_calls` devuelve `Result<PreparedToolCalls, PreparationAborted>`; `PreparationAborted { error, ordered_results }` lleva los resultados ya resueltos. | Prototipada y verificada (`...-fix-pr1-firma.patch`). |
| A | Mantener `anyhow::Result` y meter los resultados en un tipo de error que se recupera con `downcast_ref`, como ya hace el código con `StreamCancelledAfterOutput`. | No prototipada. Sin cambio de firma, pero el canal es implícito: el compilador no obliga a que los dos puntos de abortar entreguen el estado. |
| C | Parámetro de salida `&mut ordered_results` entregado por el llamador, como el patrón `new_messages_out`. | No prototipada. Añade un parámetro a 10 llamadas. |

### Alcance medido de la opción B

- Visibilidad: `prepare_tool_calls` es `pub(crate)`; el cambio no sale de `zeroclaw-runtime`. Ningún otro crate, ninguna API pública, ningún formato persistido cambia.
- Llamadores: 1 de producción (`turn/mod.rs`) y 9 tests dentro de `turn/call_prep.rs`. Solo 3 sitios de tests necesitaron edición (acceso a `error.error` y mensaje de pánico); los otros 7 compilaron sin cambios.
- Tamaño (numstat contra `65f8eb3c`): `call_prep.rs` +34/-9 y `mod.rs` +31/-6 (esto último incluye el fix simple). Sobre el fix simple, la firma añade `call_prep.rs` +34/-9 y cambia 3 líneas de `mod.rs`.
- Los dos puntos de aborto de `prepare_tool_calls` (repetición de un comando que pide aprobación y cancelación en la aprobación) devuelven el estado. Solo el segundo lo consume; en el primero el llamador descarta el estado y propaga el error, igual que hoy.
- Detalle de implementación: se escribió un `Debug` a mano para `PreparationAborted` (muestra cuántos resultados hay resueltos) en vez de derivar `Debug` en `ToolExecutionOutcome`, para no tocar otro tipo ni arriesgar que los textos de salida se impriman en logs.
- Alinea el código con `docs/book/src/architecture/tool-execution-lifecycle.md` ("Preparation fills slots for cancelled, denied, replaced, or deduplicated calls"), que hoy no se cumple al cancelar en la aprobación.

### Validación medida de la opción B

| Comprobación | Comando | Resultado |
|---|---|---|
| T1, T13 y los demás tests de la issue, más los 11 de `call_prep` | `cargo test -p zeroclaw-runtime --lib --locked -j4 -- issue_8794 call_prep --test-threads=1` | `20 passed; 0 failed` |
| Suite completa de runtime | `cargo test -p zeroclaw-runtime --lib --locked -j4` | `6085 passed; 0 failed; 10 ignored` |
| Formato | `cargo fmt --all -- --check` | limpio |
| Lint | `cargo clippy -p zeroclaw-runtime -p zeroclaw-gateway --all-targets --locked -j4 -- -D warnings` | sin avisos |

### Recomendación

Adoptar la opción B dentro del PR 1: el costo es pequeño y local (un tipo nuevo y 3 ediciones de tests), sigue siendo una sola preocupación (qué queda registrado al cancelar) y evita que el historial contradiga la interfaz. Si los mantenedores prefieren el diff mínimo, el fix simple es válido; T13 se mantendría entonces como prueba pendiente de la opción B. Decisión de los mantenedores.

## 2. Hallazgo lateral: el mensaje del usuario se persiste al terminar el turno

### Qué es y cómo se confirmó

La rama WS escribe el historial durable una sola vez por turno, al terminar (éxito, cancelación o error). Durante el turno solo se marca `running`. Evidencia en `crates/zeroclaw-gateway/src/ws.rs`: `set_session_state(..., "running", ...)` al admitir; el reemplazo con `replace_conversation_state_unless_deleted` / `persist_agent_conversation_state` solo aparece tras el turno.

T14 (`issue_8794_t14_store_holds_nothing_of_a_turn_in_flight`, medido, VERDE): mientras el turno está en vuelo y el estado es `running`, `backend.load(session_key)` devuelve vacío; tras el abort, la tienda contiene el prompt y el marcador. Comando: `cargo test -p zeroclaw-gateway --lib --locked -j4 -- issue_8794_t14 --test-threads=1`, `1 passed`.

### Consecuencias hoy

- Si el proceso muere a mitad de un turno, no queda rastro del prompt. Al arrancar, `SqliteSessionBackend::recover_abandoned_turns_at` marca como `error` los turnos en `running` y "the session's messages are untouched" (comentario del código): la sesión aparece en error sin el mensaje que la originó. VERIFICADO por lectura.
- Una recarga de página durante un turno largo lee la transcripción de la tienda; esa transcripción no incluye el prompt en curso. INFERIDO: T14 prueba la tienda, no la interfaz.
- Es un caso distinto del de este issue (Stop explícito): el abort sí persiste prompt y marcador. No es necesario para cerrar 8794.

### Precedente en el propio proyecto

El camino ACP ya hace esto: `AcpSessionStore::begin_turn_checkpoint` guarda el prompt al admitir, `append_turn_checkpoint` añade rondas, `finalize_turn_checkpoint` cierra y `recover_turn_checkpoint` recupera tras un cierre abrupto (`crates/zeroclaw-infra/src/acp_session_store.rs`). El RPC de chat y el WebSocket no lo usan. El comentario de issue 10121 describe ese trabajo para Code/ACP.

### Opciones e impacto

| Opción | Cambio | Impacto y riesgos |
|---|---|---|
| L1. No tocar y documentar | Registrar el comportamiento en `runtime-state-and-persistence.md` | Sin riesgo de código. Mantiene la pérdida del prompt ante caídas. |
| L2. Añadir el prompt al admitir con `SessionBackend::append` | 1 sitio en `ws.rs` junto al `set_session_state("running")` | Sin cambio de trait, pero: (1) `append` recrea la fila de metadatos si una eliminación concurrente (`DELETE /api/sessions/{id}`) ganó la carrera; la propia documentación del trait explica que por eso existe `replace_conversation_state_if_exists`; haría falta una variante condicionada a que la sesión exista. (2) Hay que actualizar el watermark (`PersistedWatermark`) al escribir, porque `refresh_history_if_advanced` reconstruye el agente desde la tienda cuando longitud y generación no coinciden, y esa reconstrucción descarta las rondas de herramientas en memoria (el mismo mecanismo de T11). (3) Si el proceso cae, la tienda queda con un mensaje de usuario sin respuesta; el siguiente mensaje produciría dos filas de usuario seguidas. INFERIDO que se necesita reparación; no se verificó cómo la trata el restore del WS. (4) Si `append` falla: decidir si el turno continúa o se rechaza. |
| L3. Usar el modelo de checkpoint de ACP | Métodos nuevos con implementación por defecto en `SessionBackend` (comenzar, añadir ronda, cerrar, recuperar), implementados en SQLite y JSONL, más ampliar la recuperación de arranque y el cableado en `ws.rs` | Cubre el prompt y las rondas (cierra también parte de PR 2: T4 y T10). Alcance medido: `SessionBackend` aparece en 16 archivos; las implementaciones de prueba (ws.rs 3, dispatch.rs 3, channels 15, tools 2) quedan intactas solo si los métodos nuevos tienen implementación por defecto; con métodos obligatorios habría que editar unas 23 implementaciones. Cambio de esquema en SQLite y compatibilidad de transcripciones existentes. |

### Recomendación

No incluirlo en PR 1 (una preocupación por PR). Tratarlo junto con PR 2: ambos necesitan decidir cuándo y cómo se persisten prompt y rondas durante el turno, y L3 los resuelve con el mismo mecanismo que ya usa ACP. L2 por sí sola sería un parche con tres riesgos abiertos. Decisión de diseño de los mantenedores; el cambio podría exigir un issue ordinario previo (según `rfcs.md`, una migración o un cambio de esquema por sí solos no son motivo de RFC).

## 3. Límites

- La opción B se prototipó y verificó; las opciones A y C no.
- Las opciones L2 y L3 no se implementaron; sus impactos son análisis de código más T14.
- No se ejecutó `parallel_runtime_test_gate.sh`, `cargo doc`, `./dev/ci.sh all`, la suite de `zeroclaw-channels` ni nada en Windows o en un daemon real.
- La suite de gateway, tras las modificaciones, no se volvió a correr completa; T14 y los tests de la issue sí.

## 4. Decisiones (2026-11-05)

- Se aplicó la opción B (cambio de firma) como fix definitivo de PR 1. El parche `...-fix-pr1.patch` es ahora esa variante; el fix simple se retiró del árbol.
- El hallazgo lateral queda para PR 2, junto con la persistencia durable de las rondas de herramientas (L3 es la opción de referencia). No se implementó nada de L1, L2 ni L3.
- Se corrigió H6 en un parche aparte (`...-fix-h6.patch`); ver el análisis.
