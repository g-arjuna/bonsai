import os
import paramiko
import pytest

from python.bootstrap_agent import configure_ssh_host_key_policy as bootstrap_policy
from python.inject_fault import configure_ssh_host_key_policy as fault_policy


def test_b15_ssh_default_policy_is_reject(monkeypatch):
    monkeypatch.delenv("BONSAI_SSH_HOST_KEY_POLICY", raising=False)
    monkeypatch.delenv("BONSAI_SSH_KNOWN_HOSTS", raising=False)

    for fn in (bootstrap_policy, fault_policy):
        client = paramiko.SSHClient()
        mode = fn(client)
        assert mode == "strict"
        assert isinstance(client._policy, paramiko.RejectPolicy)


def test_b15_ssh_lab_tofu_policy_must_be_explicit(monkeypatch):
    monkeypatch.setenv("BONSAI_SSH_HOST_KEY_POLICY", "lab_tofu")
    monkeypatch.delenv("BONSAI_SSH_KNOWN_HOSTS", raising=False)

    for fn in (bootstrap_policy, fault_policy):
        client = paramiko.SSHClient()
        mode = fn(client)
        assert mode == "lab_tofu"
        assert isinstance(client._policy, paramiko.AutoAddPolicy)
