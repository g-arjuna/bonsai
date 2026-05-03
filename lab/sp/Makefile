TOPO := sp-mpls-srte.clab.yml

.PHONY: up down status reset

up:
	containerlab deploy -t $(TOPO) --reconfigure

down:
	containerlab destroy -t $(TOPO) --cleanup

status:
	containerlab inspect -t $(TOPO)

reset: down up
