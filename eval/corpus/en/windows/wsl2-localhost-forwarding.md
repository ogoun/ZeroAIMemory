# WSL2 networking: localhost forwarding and mirrored mode

WSL2 runs inside a lightweight VM, so by default (NAT mode) it has its own network
interface and IP address. This leads to three distinct scenarios that behave differently.

## Windows → service in WSL

A dev server started inside WSL on port 3000 is reachable from a Windows browser at
`http://localhost:3000`. This is "localhost forwarding", on by default and controlled by
`localhostForwarding=true` in `%UserProfile%\.wslconfig`. If it stops working:

- check the service is actually listening (`ss -tlnp` inside WSL);
- some setups only forward IPv4 — try `127.0.0.1` instead of `localhost`;
- `wsl --shutdown` and start again; forwarding sometimes gets stuck after sleep.

## WSL → service on Windows

From inside WSL in NAT mode, `localhost` is the VM itself, not Windows. A database running
on Windows is reachable via the Windows host IP on the virtual network:

```bash
ip route show | awk '/default/ {print $3}'
```

Two more obstacles: the Windows service must listen on that interface (not only
`127.0.0.1`), and Windows Firewall must allow inbound traffic from the WSL virtual
adapter, which is classified as a public network.

## Other machines on the LAN → service in WSL

NAT mode does not expose WSL ports to the LAN. The classic workaround is a port proxy on
Windows (admin shell):

```
netsh interface portproxy add v4tov4 listenport=3000 listenaddress=0.0.0.0 connectport=3000 connectaddress=<wsl-ip>
```

plus a firewall rule. The WSL IP changes on restart, so this needs a script.

## Mirrored networking mode

On Windows 11 22H2 and later, mirrored mode removes most of the above:

```ini
# %UserProfile%\.wslconfig
[wsl2]
networkingMode=mirrored
```

After `wsl --shutdown`, WSL shares the Windows network interfaces: `localhost` works in
both directions, LAN clients can reach WSL services directly (subject to firewall rules
for Hyper-V), and IPv6 works. I switched all dev machines to it; the only regressions were
VPN clients that had special-cased the old NAT adapter.
