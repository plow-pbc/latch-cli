# Troubleshooting

Start with these two. The first shows this machine's state, even offline.
The second follows the host's log as it's written.

```sh
latch status --local --pretty
latch logs -f
```

## What an error means

| You see | Means | Do |
|---|---|---|
| `command not found: latch` | `~/.local/bin` isn't on your `PATH` yet. | Open a new terminal. |
| `not_logged_in` | This machine has no login. | `latch login` |
| `host_offline` | That host isn't running. | Run `latch serve -d` there. |
| `host_unreachable` | Couldn't connect. Nothing was sent. | Retry. |
| `host_required` | You have several hosts and named none. | Add `--host NAME`. |
| `serve_required` | This machine's host isn't running. | `latch serve -d` |
| `gatekeeper_unavailable` | Your rules couldn't be checked, so nothing ran. | Retry later. If the error says the account is out of credits, add Plow credits. |
| `gatekeeper_denied` | The gatekeeper refused. | For `not_covered`, ask in your Plow chat, then retry. |
| `exec_denied` | This host doesn't let agents run programs. | Asking again won't help. |
| `not_enabled` | That connector is off on the host. | On the host, run the `latch plugins enable` command the error gives. |
| `outcome_unknown` | Sent, but no answer came back. | Check before you send it again. |
| `state_format` | The state folder was made by another version of Latch. | Update Latch, or run the command the error gives. |

## Something else

**Messages or Contacts not working?** Grant Full Disk Access to Plow Latch in
System Settings, then restart the host.

**Google not working on macOS 13 or 14?** Google needs macOS 15 or later.

**A connection problem?** Run the command again with
`LATCH_TRACE=iroh=debug,latch=info` set, then read `trace.log` in the state
folder.
