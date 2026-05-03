#!/bin/sh
# host2 — traffic generator, Tenant-A (192.168.100.0/24)
ip addr add 192.168.100.102/24 dev eth1
ip route add default via 192.168.100.1 dev eth1
