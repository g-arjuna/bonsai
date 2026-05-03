#!/bin/sh
# host1 — traffic generator, Tenant-A (192.168.100.0/24)
ip addr add 192.168.100.101/24 dev eth1
ip route add default via 192.168.100.1 dev eth1
