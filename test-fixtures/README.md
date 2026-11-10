# Application test fixtures

`github-app-rsa.pem` is public, fixed test key material copied byte-for-byte
from the imported toolkit fixture. It is used only by test and runtime-fixture
configuration. Keeping an application-owned copy lets the toolkit move out of
this workspace without consumers reading its private source tree.

`github-app-rsa-2.pem` is a second public 2048-bit test key (generated for
GM-02), so configuration tests can tell which of several configured keys
signs.

These fixtures must never be configured as production GitHub App keys.
