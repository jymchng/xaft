# python-password-generator

A simple Python password generator with intentional bugs — designed as a test target for xaft.

## What it does

Generates random passwords and passphrases. Has eight documented bugs:

1. Uses `random` instead of cryptographically-secure `secrets`
2. Incomplete special-character set
3. No length validation
4. No guaranteed coverage of all requested character classes
5. `check_strength()` prints instead of returning a value
6. Inconsistent strength result format
7. Hardcoded tiny wordlist for passphrases
8. Passphrase missing separator/capitalisation options

## Running

```bash
python password_generator.py
```

## Tests

```bash
python -m pytest tests/ -v
```

## Try xaft on this project

```bash
cd /path/to/xaft-examples/python-password-generator

# Fix the security bug (random → secrets)
xaft run "Replace all uses of random.choice with secrets.choice for cryptographic security"

# Fix all bugs at once
xaft run "Fix all the bugs documented in the module docstring"

# Add a CLI interface
xaft run "Add a proper argparse CLI interface to password_generator.py so it can be used from the command line"

# Improve the wordlist
xaft run "Replace the hardcoded 5-word wordlist in generate_memorable_password with the EFF large wordlist loaded from a file"
```
