#!/bin/sh
# host3 — traffic generator, Tenant-A subinterface 0, Tenant-B subinterface 1
ip addr add 192.168.100.103/24 dev eth1
ip route add 192.168.100.0/24 via 192.168.100.1 dev eth1
