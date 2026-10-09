# Security policy

## Supported versions

PumboProx is in beta. Security fixes go into the newest release only, so update before you report.

| Version | Supported |
| --- | --- |
| newest 0.1.0 beta | yes |
| older builds | no |

## Reporting a vulnerability

Please don't report security problems in public issues, pull requests or discussions.

Report them privately on GitHub: open the **Security** tab of this repository and click **Report a vulnerability** ([direct link](https://github.com/PumboMC/PumboProx/security/advisories/new)). Only the maintainers see the report.

Please include:

- the PumboProx version (`pumboprox version`) and how you run it (binary, Docker, from source);
- the Pumpkin version and the plugins involved;
- what an attacker can do, and the steps or a proof of concept to reproduce it;
- the relevant parts of your config, without secrets.

We confirm that the report arrived, work on the fix in a private advisory and publish it together with a release. You are credited in the advisory unless you prefer not to be.

Problems in Pumpkin itself go to the [Pumpkin project](https://github.com/Pumpkin-MC/Pumpkin). Problems in one of the Pumbo plugins go to that plugin's repository, the same way.
