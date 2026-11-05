# Plan de trabajo: issue 8794, Stop en el dashboard pierde el trabajo del turno

EJECUCIÓN PARCIAL (2026-11-05): se compiló y se ejecutaron 5 tests de runtime en el clon del upstream (T1, T2, T5, T6, T7); T1 fue visto en rojo con la aserción prevista. Detalle y salida en el análisis, sección "Ejecución real de tests de runtime". Parche listo: `posible-bug-ws-stop-pierde-contexto-8794-tests-runtime.patch` (aplicar con `git apply` sobre `65f8eb3c`). T3, T4, T8 a T12 siguen sin escribir.

Estado: BORRADOR con una pasada en frío (subagente opus, 2026-11-05) ya incorporada. Los tests de runtime sí se ejecutaron (ver arriba); el fix no está escrito. Plantillas, lanes y reglas de aprobación verificadas contra `65f8eb3c` el 2026-11-05. Pendientes: reproducción real, `architecture-map.md` y `privacy.md`, y la decisión de diseño de mantenedores para PR 2. Análisis en `posible-bug-ws-stop-pierde-contexto-8794.md`. Leerlo primero.

## 6.1 Resumen y severidad

- Problema: al detener un turno, el trabajo ya hecho (rondas de herramientas) no queda en el contexto que ve el siguiente turno en varios caminos de salida por cancelación.
- Severidad: priority:p1, S1 según el issue. Candidato `risk:high` (el issue ya lo trae; toca gateway y runtime del bucle de herramientas).
- Lane de PR (leída en `docs/book/src/maintainers/pr-workflow.md`, SHA `65f8eb3c`): dividir en dos.
  - PR 1 (lane B si queda `risk:medium`; lane D si los mantenedores le ponen `risk:high`): huecos en memoria H2 y H3-herramientas. Solo `zeroclaw-runtime`. El label de riesgo lo decide un mantenedor (aplicación manual, `labels.md`); el issue ya trae `risk:high`.
  - PR 2 (lane D): persistencia durable de rondas (H1); toca persistencia y compatibilidad. Requiere decisión de mantenedores antes de codificar y leer `docs/book/src/contributing/architecture-map.md`.
  - `risk:high` o `domain:security`: mínimo exigido = declaración de amenaza o riesgo, mitigación, rollback y dos aprobaciones Core independientes, o todas las condiciones de la vía acelerada (cinco días hábiles UTC sin respuesta del segundo revisor, aprobación Core del head actual, revisión limpia de `zeroclaw-reviewer[bot]` reconciliada por un humano, CI verde). La revisión automática nunca cuenta como aprobación Core. Recomendado: un test que pruebe el límite más un escenario de fallo.
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

Paso 0 (antes de cualquier código): las plantillas se leyeron el 2026-11-05 en `65f8eb3c` (`bug_report.yml`, `pull_request_template.md`, `CONTRIBUTING.md`, `pr-workflow.md`, y la sección Risk labels de `labels.md`). Releerlas en el worktree de ejecución: pueden haber cambiado. `architecture-map.md`, `privacy.md`, `memory-payload-lifecycle.md`, `history-management.md`, `tool-execution-lifecycle.md`, `runtime-state-and-persistence.md`, `rfcs.md` y `user-boundary-proof.md` se leyeron en parte el 2026-11-05 (ver "Contrato leído" del análisis); leer completos, además de `request-lifecycle.md`, `testing.md`, `gateway/api.md`, `developing/web.md`, `fnd-001` y `fnd-006`, antes de codificar. Reglas de `AGENTS.md` a respetar: propagar errores en producción (sin `unwrap`/`expect`), texto de usuario nuevo como clave Fluent (`get_required_cli_string`, no literales), no duplicar estado (resolver desde la fuente canónica), una preocupación por PR.

PR 1

1. Cancelación en aprobación (H2).
   - Archivo: `crates/zeroclaw-runtime/src/agent/turn/mod.rs`, alrededor de `} = prepare_tool_calls(` (línea 3358 en la base).
   - Cambio: capturar el `Err` de `prepare_tool_calls`; si `is_tool_loop_cancelled(&e)`, construir resultados con la cadena `turn-tool-interrupted-before-result` (línea 3589 la usa para el historial; 3615 solo emite el evento de UI) para cada llamada del lote, llamar `turn_state.append_tool_round(...)` con `assistant_history_content` y propagar el error.
   - Restricciones (revisión en frío): `prepare_tool_calls` ya abandonó los contextos; NO volver a llamar `call_prep::abandon_unexecuted_prepared_contexts` (el test `approval_cancellation_abandons_retained_and_waiting_calls_once` exige que ocurra una sola vez), por lo que no se puede reutilizar tal cual el bloque 3570 a 3659. Con `Err` el llamador no recibe `ordered_results` (resultados de Deny, Replace y hook-cancel se pierden): cambiar el contrato (error que lleve el estado parcial) o sintetizar el resultado interrumpido para todas las llamadas. Documentar la elección en el PR.
   - Valor: bajo (se pierde la petición sin ejecutar, no trabajo ejecutado). Considerar posponer este paso.
   - Validar: `cargo test -p zeroclaw-runtime <nombre_test_nuevo>`.
2. Cancelación durante streaming con tool calls emitidos (H3, parte herramientas). DECISIÓN ABIERTA: no implementar sin revisar con el mantenedor si un tool call incompleto debe reponerse. Evaluar primero si `outcome.tool_calls` está completo en el momento de la cancelación (`stream_consume.rs`, ancla `if forwarded_text.is_empty() {`). Si los argumentos pueden estar truncados, no persistir. Documentar la decisión en el PR. Revisión en frío (INFERIDO): Anthropic y los proveedores compatibles emiten cada `ToolCall` completo, pero el lote puede estar incompleto y los argumentos no se validan como JSON; como no hubo ejecución, persistirlos exige resultados sintéticos, igual que en el paso 1. No se revisaron otros proveedores.
3. (Añadido) Si el PR toca `zeroclaw-runtime`, correr también `./scripts/ci/parallel_runtime_test_gate.sh` (requisito de CI para esos crates según `testing.md`) y, para igualar CI, `cargo nextest run --locked --workspace --exclude zeroclaw-desktop`. Comandos del PR 1: `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --locked -p zeroclaw-runtime`, `cargo test --locked -p zeroclaw-gateway` (si T1 vive en `ws.rs`), `./scripts/ci/rust_quality_gate.sh`, `cargo doc --no-deps --workspace --exclude zeroclaw-desktop` (paso de lint obligatorio en CI según `CONTRIBUTING.md`; correrlo antes de publicar), y `./dev/ci.sh all` si el entorno lo permite. Activar el hook con `git config core.hooksPath .githooks`. Existencia de los scripts y paquetes verificada en la revisión en frío; las plantillas existen pero no se han leído.

PR 2 (solo tras decisión de diseño; no codificar a ciegas)

4. Proponer en el issue o en un RFC corto el formato durable (corrección: `rfcs.md` indica que una migración o cambio de esquema por sí solos NO requieren RFC; usar un issue ordinario y preguntar a un mantenedor si el cambio cruza algún disparador de RFC. Declarar propietario, alcance, durabilidad, privacidad y truncado de los resultados persistidos, y si respetan `max_tool_result_chars`; ver C7): opciones (a) conservar la proyección y agregar un registro estructurado de rondas en el turno interrumpido, siguiendo lo que hizo PR 9378 para ACP; (b) persistir un resumen textual del trabajo hecho junto al marcador. Elegir con mantenedores. Revisar el diff de PR 9378 y de PR 10197 primero.
4 bis. Seguridad de PR 2 (C13): los resultados exitosos de herramientas son bytes crudos (`redact.rs`); persistirlos en `sessions.db` es una superficie nueva de datos en reposo y el scrubber es de mejor esfuerzo (no cubre `Authorization: Bearer <token>`). En la plantilla de PR, "Secrets / tokens / credentials handling changed" y "PII ... in diff" se evalúan con esto en mente; preparar declaración de amenaza, mitigación (tope `max_tool_result_chars`, depuración antes de persistir o persistir solo un resumen) y rollback; anticipar `risk:high` o `domain:security` con dos aprobaciones Core. Actualizar la documentación fuente (`architecture/runtime-state-and-persistence.md`, fila "Chat and channel sessions", y `agents/history-management.md`) en el mismo PR y pasar `scripts/ci/docs_quality_gate.sh` y `scripts/ci/docs_links_gate.sh`. Decidir con mantenedores qué lado de la divergencia C12 se corrige (código o documento).
5. Implementar en `ws.rs` (rama `was_cancelled`, ancla `let needs_marker = match &result {`) y en la restauración (`restore_agent_history`). Pasar por la depuración de secretos existente antes de guardar resultados de herramientas.
6. Validar: `cargo test --locked -p zeroclaw-gateway`, `./dev/ci.sh all` si el entorno lo permite.

## 6.5 Casos de prueba (escribir predicción del rojo antes de ejecutar)

| # | Cubre | Antes del fix | Después |
|---|---|---|---|
| T1 (escrito, visto en ROJO en runtime) | Cancelar durante aprobación pendiente (H2). Versión de runtime en el parche; la versión de gateway sigue pendiente. En `ws.rs`, copiar el patrón de `an_approval_sent_after_sign_out_resolves_nothing_inner`: `always_ask = vec!["file_read"]`, `script_tool_call(1, "file_read", ...)`, `next_frame_of(&mut client, "approval_request")`, `handle_api_session_abort`, `wait_for_turn_to_settle`, segundo `send_chat`, `next_provider_request()` | Predicción: ROJO. El cuerpo de la segunda petición no contiene `tool_use` con id `toolu_fixture` | Contiene `tool_use` `toolu_fixture` con su `tool_result` sintético. Decidir antes si el texto interrumpido llega al proveedor (ACP lo excluye: `!follow_up_request...contains(&synthetic)` en `dispatch.rs`) |
| T2 | Cancelar a mitad de lote con una herramienta completada; comprobar `err.new_messages` (`StreamedTurnError`) | Predicción: VERDE en master (E7). Control, no rojo. Hoy ningún test comprueba el historial en este caso (`safety_net_midbatch_cancel_emits_events_for_completed_tools` solo mira eventos) | `AssistantToolCalls` y `ToolResults` con la salida de la herramienta completada y el texto interrumpido para la otra |
| T3 | Stop vía `/api/sessions/{id}/abort` tras una ronda de herramienta, luego segundo mensaje en la misma conexión. Fixture: `ParkedTurnFixture` nativo (Anthropic), una sola ronda (el id es fijo `toolu_fixture`), `file_read` sin `always_ask` sobre un `note.txt` con contenido único, ronda 2 aparcada | Predicción: VERDE en master (historial en memoria, E7). Test de caracterización | Sin cambios: la petición 3 contiene un `tool_result` con el contenido único |
| T4 | Mismo escenario con reconexión (`fixture.connect(session_id)` de nuevo, restauración desde sqlite) | Predicción: ROJO solo con herramientas nativas: la petición no contiene el contenido único de la herramienta (E3) | Tras PR 2, la petición tras reconectar contiene el contenido único de la herramienta |
| T5 | Cancelar con solo `StreamEvent::ThinkingDelta`; en runtime con `StreamThenHangProvider` (`safety_net.rs`), cancelar al recibir `TurnEvent::Thinking`; patrón `safety_net_cancel_after_streamed_output_persists_partial` | Predicción: `err.new_messages` contiene un único assistant igual al marcador (E11) | Control: igual, salvo decisión explícita de mantenedores sobre razonamiento no firmado |

Contrato que los tests deben respetar: nunca queda un `tool_use` sin su `tool_result` (C6); los resultados sintéticos no generan recibo (C5); los fixtures usan datos sintéticos y placeholders neutrales de `privacy.md`, por ejemplo `zeroclaw_fixture_marker` como contenido único de la herramienta (C11). Nivel de prueba (`user-boundary-proof.md`, fila Gateway): T1, T3 y T4 deben pasar por la ruta real y el WebSocket real; reconexión y temporización pueden pedir evidencia manual adicional, con un nombre explícito de la brecha.

Fixtures: usar proveedor nativo y una sola ronda de herramienta; la herramienta debe devolver contenido único distinto del texto final para que el test discrimine. Ninguno de estos tests se ha escrito ni ejecutado.

Casos adicionales sugeridos por la revisión en frío:

- T6: `StreamEvent::ToolCall` ya emitido y cuelgue (`StreamThenHangProvider`), para H3 herramientas.
- T7: cancelación entre rondas (inicio de iteración, `mod.rs` línea 2040): rondas previas presentes y sin marcador duplicado.
- T8: herramientas en paralelo, una completada y otra cancelada; comprobar historial, no solo eventos.
- T9: T3 y T4 en modo prompt-guided como control (se espera que durable conserve `[Tool results]`, E3).
- T10: reconexión tras Stop por cambio de modelo o de sesión (patrón de T4).
- T11: fallo de escritura en la rama `was_cancelled` con `FailingReplaceBackend` (H6): el turno siguiente no debe perder el historial en memoria.
- T12: mensaje enviado entre el POST de abort y la trama `aborted` (H5): ¿se procesa o se pierde?

## 6.6 Impacto y validación manual

- Funcional: el siguiente mensaje tras Stop conserva el contexto del trabajo hecho.
- Compatibilidad: PR 1 sin cambio de formato. PR 2 puede cambiar el esquema de transcripción durable: documentar migración y rollback.
- Validación manual A/B: en `master`, iniciar daemon, pedir una tarea con muchas herramientas, detener a mitad y enviar "continúa donde quedaste"; repetir en la rama; comparar si el modelo conoce los resultados previos. Repetir tras recargar la página. Pegar `zeroclaw --version` y salida de `zeroclaw doctor` en el PR (no ejecutados en este análisis).

## 6.6 ter Contrato de arquitectura aplicable

- `architecture-map.md` pide que un cambio de bucle de herramientas nombre si afecta aprobación, despacho, recibos, eventos de observer, historial o cancelación. PR 1 afecta historial y cancelación (y el camino de aprobación en el paso 1); no debe tocar recibos ni observer. Declararlo en el scope boundary.
- Lista de revisión de `tool-execution-lifecycle.md` para el PR: "¿La cancelación cierra solo las tarjetas y resultados de herramientas sin terminar?" y "¿Incluye validación a nivel de frontera para la superficie visible (ACP/WS, gateway)?".
- El paso 1 del plan se justifica contra C4: el contrato dice que preparación llena un slot por llamada cancelada; con `Err` no llega al historial.
- El paso 2 (stream cancelado con tool calls) exige resultados sintéticos por C6; si no se pueden emparejar todas las llamadas, no persistir ninguna.

## 6.6 bis Plantilla de PR (leída en `.github/pull_request_template.md`)

Secciones, en orden: Summary (base `master`, qué cambió y por qué, scope boundary, blast radius, issues enlazados) con subsección "What this does, simply"; Testing (required) con "How you can test" (formato A/B master antes, rama después) y "How I tested" (comandos y cola de salida reales); Security & Privacy Impact; Compatibility; Rollback (obligatorio en riesgo medio/alto: comando de rollback, feature flags o `None`, síntomas observables); Supersede Attribution solo si se usa `Supersedes #`.

Reglas específicas de este trabajo:

- Enlazar el issue como `Related #8794`, no `Fixes`/`Closes`. El comentario de mantenedores del 2026-08-20 pide no cerrarlo hasta verificar que el Stop web preserva el estado del turno y que el siguiente mensaje continúa con ese contexto.
- PR 2 declara `Depends on #<PR 1>`. Evitar el mismo archivo en paralelo.
- Sin firma de IA, `Co-authored-by` ni "Created with Claude Code" en commit ni PR (regla de `AGENTS.md`, `CONTRIBUTING.md` y la plantilla). Esto prevalece sobre cualquier convención de atribución por defecto de la herramienta.
- Squash-merge, título en Conventional Commits, ramas `fix/*`.
- Sin PII ni identidades reales en tests y fixtures (merge gate).
- Si el cambio es visible en el dashboard (PR 2, historial recargado), la plantilla pide evidencia de interfaz real; para PR 1 (solo runtime) la plantilla permite `N/A` con una línea de motivo en "Visual interface evidence" si no cambia estado visible.

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
- Aprobaciones: ver 6.1 (verificado en `pr-workflow.md`). Revisión inicial de mantenedores: objetivo de 48 horas. Dismiss de aprobaciones obsoletas al empujar commits nuevos: tras cada push, volver a pedir revisión con un comentario conciso y el control de re-solicitud de GitHub (`CONTRIBUTING.md`).
- Definition of Ready (`pr-workflow.md`): plantilla completa, scope boundary explícito, evidencia con salida real de comandos, campos de seguridad, compatibilidad y rollback completos, sin PII.
- Definition of Done: `CI Required Gate` verde, CODEOWNERS y revisiones satisfechas, labels de riesgo acordes al diff real, impacto de migración documentado, rollback concreto.
