package main

func receive(ch <-chan int) {
	select {
	case value := <-ch:
		consume(value)
	}
	after()
}
