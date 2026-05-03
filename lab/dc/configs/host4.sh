#!/bin/sh
# host4 — traffic generator, Tenant-A
ip addr add 192.168.100.104/24 dev eth1
ip route add 192.168.100.0/24 via 192.168.100.1 dev eth1
