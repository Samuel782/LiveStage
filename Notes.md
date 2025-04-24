#  Note

## Get e set in un protocollo
> get e set in un protocollo indicano che le proprietà posso:
- get: essere lette.
- set: essere modificate.
 
## Differenza tra some e any:
> sono keywords per indicare tipi astratti
- any indica un qualsiasi oggetto che è conforme ad un certo protocollo.
- some indica un oggetto concreto che è conforme ad un protocollo, ma il tipo preciso viene nascosto all’esterno.

## Type Erasure in Swift
- Usa type erasure quando:
	-	Hai un protocollo con associatedtype o metodi che ritornano some View.
	-	Vuoi creare un array eterogeneo con più oggetti conformi a quel protocollo.
	-	Swift non ti permette di usare direttamente il protocollo come tipo ([MyProtocol]).