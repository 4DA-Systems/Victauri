## Default Permission

Default permissions for the Victauri plugin — the commands Victauri's own injected bridge needs (the eval callback) plus the synchronous, read-only introspection commands. `allow-victauri-eval-js` and `allow-victauri-dom-snapshot` (page-callable eval / snapshot; async commands) are NOT included — grant them explicitly if page code needs them.

#### This default permission set includes the following:

- `allow-victauri-eval-callback`
- `allow-victauri-get-window-state`
- `allow-victauri-list-windows`
- `allow-victauri-get-ipc-log`
- `allow-victauri-get-registry`
- `allow-victauri-get-memory-stats`
- `allow-victauri-verify-state`
- `allow-victauri-detect-ghost-commands`
- `allow-victauri-check-ipc-integrity`

## Permission Table

<table>
<tr>
<th>Identifier</th>
<th>Description</th>
</tr>


<tr>
<td>

`victauri-plugin:allow-victauri-check-ipc-integrity`

</td>
<td>

Enables the victauri_check_ipc_integrity command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`victauri-plugin:deny-victauri-check-ipc-integrity`

</td>
<td>

Denies the victauri_check_ipc_integrity command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`victauri-plugin:allow-victauri-detect-ghost-commands`

</td>
<td>

Enables the victauri_detect_ghost_commands command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`victauri-plugin:deny-victauri-detect-ghost-commands`

</td>
<td>

Denies the victauri_detect_ghost_commands command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`victauri-plugin:allow-victauri-dom-snapshot`

</td>
<td>

Enables the victauri_dom_snapshot command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`victauri-plugin:deny-victauri-dom-snapshot`

</td>
<td>

Denies the victauri_dom_snapshot command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`victauri-plugin:allow-victauri-eval-callback`

</td>
<td>

Enables the victauri_eval_callback command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`victauri-plugin:deny-victauri-eval-callback`

</td>
<td>

Denies the victauri_eval_callback command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`victauri-plugin:allow-victauri-eval-js`

</td>
<td>

Enables the victauri_eval_js command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`victauri-plugin:deny-victauri-eval-js`

</td>
<td>

Denies the victauri_eval_js command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`victauri-plugin:allow-victauri-get-ipc-log`

</td>
<td>

Enables the victauri_get_ipc_log command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`victauri-plugin:deny-victauri-get-ipc-log`

</td>
<td>

Denies the victauri_get_ipc_log command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`victauri-plugin:allow-victauri-get-memory-stats`

</td>
<td>

Enables the victauri_get_memory_stats command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`victauri-plugin:deny-victauri-get-memory-stats`

</td>
<td>

Denies the victauri_get_memory_stats command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`victauri-plugin:allow-victauri-get-registry`

</td>
<td>

Enables the victauri_get_registry command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`victauri-plugin:deny-victauri-get-registry`

</td>
<td>

Denies the victauri_get_registry command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`victauri-plugin:allow-victauri-get-window-state`

</td>
<td>

Enables the victauri_get_window_state command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`victauri-plugin:deny-victauri-get-window-state`

</td>
<td>

Denies the victauri_get_window_state command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`victauri-plugin:allow-victauri-list-windows`

</td>
<td>

Enables the victauri_list_windows command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`victauri-plugin:deny-victauri-list-windows`

</td>
<td>

Denies the victauri_list_windows command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`victauri-plugin:allow-victauri-verify-state`

</td>
<td>

Enables the victauri_verify_state command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`victauri-plugin:deny-victauri-verify-state`

</td>
<td>

Denies the victauri_verify_state command without any pre-configured scope.

</td>
</tr>
</table>
