"""
Simple password generator with several intentional issues for xaft to find and fix.
"""
import random
import string


# BUG 1: `random` is not cryptographically secure — should use `secrets`
def generate_password(length=8, use_uppercase=True, use_digits=True, use_special=True):
    """Generate a random password."""
    chars = string.ascii_lowercase

    if use_uppercase:
        chars += string.ascii_uppercase
    if use_digits:
        chars += string.digits
    if use_special:
        # BUG 2: missing several special characters; incomplete set
        chars += "!@#$"

    # BUG 3: no validation that `length` is a positive integer
    # BUG 4: no guarantee that at least one char from each requested category is included
    password = ""
    for i in range(length):
        password += random.choice(chars)

    return password


# BUG 5: function name is misleading — this checks strength but returns nothing useful
def check_strength(password):
    """Check password strength."""
    score = 0
    if len(password) >= 8:
        score += 1
    if any(c.isupper() for c in password):
        score += 1
    if any(c.isdigit() for c in password):
        score += 1
    if any(c in string.punctuation for c in password):
        score += 1

    # BUG 6: inconsistent return — should return a structured result
    if score == 4:
        print("Strong")
    elif score >= 2:
        print("Medium")
    else:
        print("Weak")


def generate_memorable_password(words=3):
    """Generate a memorable passphrase from random words."""
    # BUG 7: hardcoded tiny word list — should load from a real wordlist
    wordlist = ["apple", "blue", "car", "dog", "egg"]
    # BUG 8: no separator or capitalisation option
    return "".join(random.choice(wordlist) for _ in range(words))


if __name__ == "__main__":
    print("=== Password Generator ===")
    pw = generate_password(16)
    print(f"Generated: {pw}")
    check_strength(pw)

    phrase = generate_memorable_password()
    print(f"Passphrase: {phrase}")
