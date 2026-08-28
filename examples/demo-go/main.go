package main

import "fmt"

func greet(name string) string {
	return fmt.Sprintf("Hello, %s!", name)
}

func farewell(name string) string {
	return fmt.Sprintf("Goodbye, %s!", name)
}

func main() {
	fmt.Println(greet("world"))
	fmt.Println(farewell("world"))
}
