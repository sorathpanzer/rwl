# FreeBSD

## Screen lock (`rwl msg lock`)

The native lock checks your password with PAM. On FreeBSD, OpenPAM's `pam_unix`
can only read password hashes as root (`/etc/master.passwd` is root-only), and
unlike Linux there is no `unix_chkpwd` helper. A compositor running as your user
would therefore reject every password, including the correct one.

To work around this, rwl re-runs **itself** through `doas` for each unlock
attempt, as `doas -n /path/to/rwl __chkpwd`. No extra binary is involved. In
that mode rwl does only one thing and then exits: it reads the password from a
pipe and checks it against PAM for the user doas reports in `DOAS_USER`, so you
can only check your own password. After a wrong password it waits 2 s.

One-time setup:

1. Install rwl to a **root-owned** location. A `nopass` doas rule on a binary
   you can write to would let anything running as you become root, so rwl
   refuses to lock from such a path:

   ```
   cargo build --release
   doas install -o root -g wheel -m 0555 target/release/rwl /usr/local/bin/rwl
   ```

2. Add this rule at the **end** of `/usr/local/etc/doas.conf` (doas uses the
   last matching rule). Replace `youruser` with your user name:

   ```
   permit nopass youruser as root cmd /usr/local/bin/rwl args __chkpwd
   ```

3. Install the PAM service:

   ```
   doas install -o root -g wheel -m 0644 pam/rwl-lock /usr/local/etc/pam.d/rwl-lock
   ```

4. Start rwl from `/usr/local/bin/rwl`. The doas rule matches that exact path.

If any of this is missing, rwl **refuses to lock** and shows a red 5-second
notice on the bar that says what is wrong. A broken setup would otherwise lock
you out with no way back in.
