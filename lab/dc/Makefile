TOPO := dc-evpn-srv6.clab.yml

.PHONY: up down status reset

up:
	containerlab deploy -t $(TOPO) --reconfigure

down:
	containerlab destroy -t $(TOPO) --cleanup

status:
	containerlab inspect -t $(TOPO)

reset: down up
