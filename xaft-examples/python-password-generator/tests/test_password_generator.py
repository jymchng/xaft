"""Tests for password_generator.py"""
import sys
import os

sys.path.insert(0, os.path.dirname(os.path.dirname(__file__)))
import password_generator


def test_generate_password_default_length():
    pw = password_generator.generate_password()
    assert len(pw) == 8


def test_generate_password_custom_length():
    pw = password_generator.generate_password(length=20)
    assert len(pw) == 20


def test_generate_password_only_lowercase():
    pw = password_generator.generate_password(
        length=50, use_uppercase=False, use_digits=False, use_special=False
    )
    assert all(c.islower() for c in pw)


def test_generate_password_contains_digits():
    # Run many times to reduce flakiness
    found_digit = any(
        any(c.isdigit() for c in password_generator.generate_password(length=20, use_digits=True))
        for _ in range(20)
    )
    assert found_digit


def test_check_strength_does_not_crash():
    # Just verifies no exception — the function currently prints instead of returning
    password_generator.check_strength("Abc123!@")
    password_generator.check_strength("abc")
    password_generator.check_strength("")


def test_generate_memorable_password():
    phrase = password_generator.generate_memorable_password(words=3)
    assert isinstance(phrase, str)
    assert len(phrase) > 0
