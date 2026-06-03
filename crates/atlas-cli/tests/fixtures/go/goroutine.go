package main

import "os"

func main() {
	go func() {
		f, _ := os.Open("file.txt")
		f.Close()
	}()
}
