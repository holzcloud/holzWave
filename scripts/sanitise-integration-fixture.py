#!/usr/bin/env python3
"""Make the output of a command-line tool safe to commit as a test fixture.

Reads standard input and writes standard output. It never touches a file.

JSON input is walked and keeps its keys and structure:
  * every GUID becomes a synthetic one (00000000-0000-4000-8000-000000000001, ...) in order of first appearance; a GUID
    that is already in the synthetic range stays;
  * a string under the key name, displayName, tenantDisplayName or domain becomes "Item N" of that key, except a few
    well-known values of the tools (for example the name of a public cloud);
  * a string that holds an at sign becomes user@example.com;
  * a string of 32 or more characters of [A-Za-z0-9+/_=-] becomes REDACTED (an absolute path and a commit or digest
    in lowercase hex are kept);
  * the home directory of this machine becomes /home/user.
Text input gets the home directory, e-mail addresses and GUIDs replaced the same way.

Options:
  --keep-names        leave the values under name and the like alone (a kubeconfig's context names are not identities)
  --strict            JSON only: every string value is replaced (a GUID by a synthetic one, an address by user@example.com,
                      anything else by "Item N" of its key) except the values of the keys state, cloudName,
                      environmentName and type; only the structure of the tool's answer survives
  --redact-exec-env   replace the values of users[].user.exec.env[] (kubectl does not redact them)
  --talos             addresses become 192.0.2.N, host names node-N, certificate-looking strings REDACTED
  --indent N          indentation of JSON output (default 2)

Run it as `python3 -I scripts/sanitise-integration-fixture.py`. It exits 1 with a message on input it cannot read.
"""

import json
import os
import re
import sys

GUID = re.compile(r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}")
SYNTHETIC_GUID = re.compile(r"00000000-0000-4000-8000-0000000000[0-9a-f]{2}")
EMAIL = re.compile(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}")
LONG_WORD = re.compile(r"[A-Za-z0-9+/_=-]{32,}")
HEX_DIGEST = re.compile(r"[0-9a-f]{40}|[0-9a-f]{64}")
IPV4 = re.compile(r"\b(?:\d{1,3}\.){3}\d{1,3}\b")
IPV6 = re.compile(r"[0-9a-fA-F:.]+")
NAME_KEYS = {"name", "displayName", "tenantDisplayName", "domain"}
HOST_KEYS = {"hostname", "nodename", "fqdn"}
KEEP_VALUES = {"AzureCloud", "AzureUSGovernment", "AzureChinaCloud", "AzureGermanCloud", "Enabled", "Succeeded"}
STRICT_KEEP_KEYS = {"state", "cloudName", "environmentName", "type"}
SYNTHETIC_EMAIL = "user@example.com"


class Sanitiser:
    def __init__(self, talos=False, redact_exec_env=False, keep_names=False, strict=False):
        self.strict = strict
        self.keep_names = keep_names
        self.talos = talos
        self.redact_exec_env = redact_exec_env
        self.guids = {}
        self.items = {}
        self.addresses = {}
        self.addresses6 = {}
        self.hosts = {}
        self.host_aliases = {}
        self.homes = self._homes()

    @staticmethod
    def _homes():
        homes = set()
        for candidate in (os.environ.get("HOME"), os.path.expanduser("~")):
            if candidate and len(candidate) > 4 and candidate not in ("/home/user",):
                homes.add(candidate.rstrip("/"))
        # Longest first so that a nested path is replaced whole.
        return sorted(homes, key=len, reverse=True)

    def guid(self, match):
        key = match.group(0).lower()
        if SYNTHETIC_GUID.fullmatch(key):
            return key
        if key not in self.guids:
            self.guids[key] = "00000000-0000-4000-8000-%012d" % (len(self.guids) + 1)
        return self.guids[key]

    def item(self, key, value):
        table = self.items.setdefault(key, {})
        if value not in table:
            table[value] = "Item %d" % (len(table) + 1)
        return table[value]

    def address(self, match):
        value = match.group(0)
        if value not in self.addresses:
            self.addresses[value] = "192.0.2.%d" % (len(self.addresses) + 1)
        return self.addresses[value]

    def host(self, value):
        if value not in self.hosts:
            self.hosts[value] = "node-%d" % (len(self.hosts) + 1)
            # The short name (first label) of a host name stands for the same node when a whole string is exactly it.
            self.host_aliases.setdefault(value.split(".")[0], self.hosts[value])
        return self.hosts[value]

    def ipv6(self, value):
        """A whole string that is an IPv6 address becomes a documentation address (2001:db8::N)."""
        if value.count(":") >= 3 or "::" in value:
            if IPV6.fullmatch(value):
                if value not in self.addresses6:
                    self.addresses6[value] = "2001:db8::%d" % (len(self.addresses6) + 1)
                return self.addresses6[value]
        return None

    def collect_hosts(self, value, key=None):
        """Talos: learn the host names before any string is rewritten (a member's id is its host name)."""
        if isinstance(value, dict):
            metadata = value.get("metadata")
            spec = value.get("spec")
            if isinstance(metadata, dict) and str(metadata.get("type", "")).startswith("Members") and isinstance(spec, dict):
                if isinstance(spec.get("hostname"), str):
                    self.host(spec["hostname"])
                known = self.hosts.keys() | self.host_aliases.keys()
                if isinstance(metadata.get("id"), str) and metadata["id"] not in known:
                    self.host(metadata["id"])
            for k, v in value.items():
                self.collect_hosts(v, k)
        elif isinstance(value, list):
            for v in value:
                self.collect_hosts(v, key)
        elif isinstance(value, str) and key in HOST_KEYS and value:
            self.host(value)

    def text(self, value):
        """Replace the home directory, e-mail addresses and GUIDs in free text."""
        for home in self.homes:
            value = value.replace(home, "/home/user")
        value = re.sub(r"[A-Za-z]:\\Users\\[^\\\s\"']+", "C:\\\\Users\\\\user", value)
        value = EMAIL.sub(SYNTHETIC_EMAIL, value)
        value = GUID.sub(self.guid, value)
        if self.talos:
            value = IPV4.sub(self.address, value)
            for original, replacement in self.hosts.items():
                value = value.replace(original, replacement)
        return value

    def strict_string(self, value, key):
        if key in STRICT_KEEP_KEYS:
            return value
        if GUID.fullmatch(value):
            return GUID.sub(self.guid, value)
        if EMAIL.fullmatch(value):
            return SYNTHETIC_EMAIL
        if not value:
            return value
        return self.item(key or "value", value)

    def string(self, value, key=None):
        if self.strict:
            return self.strict_string(value, key)
        if value in KEEP_VALUES:
            return value
        if self.talos and value:
            if key in HOST_KEYS or value in self.hosts:
                return self.host(value)
            if value in self.host_aliases:
                return self.host_aliases[value]
            address = self.ipv6(value)
            if address:
                return address
        if "@" in value and EMAIL.search(value):
            return SYNTHETIC_EMAIL if EMAIL.fullmatch(value) else self.text(value)
        if key in NAME_KEYS and not self.keep_names and value and not GUID.fullmatch(value):
            return self.item(key, value)
        if self.talos and len(value) >= 32 and re.search(r"[A-Za-z0-9+/=]{32,}", value) and not value.startswith("/"):
            if not HEX_DIGEST.fullmatch(value) and not GUID.search(value):
                return "REDACTED"
        if (
            not GUID.search(value)
            and LONG_WORD.fullmatch(value)
            and not value.startswith("/")
            and not HEX_DIGEST.fullmatch(value)
        ):
            return "REDACTED"
        return self.text(value)

    def walk(self, value, key=None):
        if isinstance(value, dict):
            return {k: self.walk(v, k) for k, v in value.items()}
        if isinstance(value, list):
            return [self.walk(v, key) for v in value]
        if isinstance(value, str):
            return self.string(value, key)
        return value

    def exec_env(self, config):
        users = config.get("users") if isinstance(config, dict) else None
        for entry in users or []:
            env = ((entry.get("user") or {}).get("exec") or {}).get("env") if isinstance(entry, dict) else None
            for pair in env or []:
                if isinstance(pair, dict) and "value" in pair:
                    pair["value"] = "REDACTED"
        return config


def objects_in(text):
    """The JSON documents of a stream of back-to-back objects, or None when the text is not such a stream."""
    decoder = json.JSONDecoder()
    position = 0
    found = []
    while position < len(text):
        while position < len(text) and text[position].isspace():
            position += 1
        if position >= len(text):
            break
        try:
            value, position = decoder.raw_decode(text, position)
        except ValueError:
            return None
        found.append(value)
    return found or None


def main(argv):
    talos = "--talos" in argv
    redact_exec_env = "--redact-exec-env" in argv
    keep_names = "--keep-names" in argv
    strict = "--strict" in argv
    indent = 2
    if "--indent" in argv:
        try:
            indent = int(argv[argv.index("--indent") + 1])
        except (IndexError, ValueError):
            print("sanitise-integration-fixture: --indent needs a number", file=sys.stderr)
            return 1
    try:
        raw = sys.stdin.buffer.read().decode("utf-8")
    except (OSError, UnicodeDecodeError) as error:
        print("sanitise-integration-fixture: cannot read the input: %s" % error, file=sys.stderr)
        return 1
    sanitiser = Sanitiser(talos=talos, redact_exec_env=redact_exec_env, keep_names=keep_names, strict=strict)
    stripped = raw.lstrip()
    documents = objects_in(raw) if stripped[:1] in ("{", "[") else None
    if documents is not None:
        out = []
        for document in documents:
            if redact_exec_env:
                document = sanitiser.exec_env(document)
            if talos:
                sanitiser.collect_hosts(document)
            out.append(json.dumps(sanitiser.walk(document), indent=indent, ensure_ascii=False))
        sys.stdout.write("\n".join(out) + "\n")
    else:
        sys.stdout.write(sanitiser.text(raw))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
