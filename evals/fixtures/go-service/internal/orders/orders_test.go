package orders

import "testing"

func TestSubtotal(t *testing.T) {
	o := Order{Items: []Item{{SKU: "a", Quantity: 2, UnitCents: 500}, {SKU: "b", Quantity: 1, UnitCents: 250}}}
	if got := o.Subtotal(); got != 1250 {
		t.Fatalf("Subtotal() = %d, want 1250", got)
	}
}

func TestTotalAppliesDiscountOnce(t *testing.T) {
	o := Order{Items: []Item{{SKU: "a", Quantity: 1, UnitCents: 1000}}, DiscountPct: 10}
	if got := o.Total(); got != 900 {
		t.Fatalf("Total() = %d, want 900", got)
	}
}

func TestCreateRejectsEmptyOrders(t *testing.T) {
	s := NewService(NewMemoryStore())
	if _, err := s.Create(nil, 0); err != ErrEmptyOrder {
		t.Fatalf("err = %v, want ErrEmptyOrder", err)
	}
}

func TestCreateThenGet(t *testing.T) {
	s := NewService(NewMemoryStore())
	o, err := s.Create([]Item{{SKU: "a", Quantity: 1, UnitCents: 100}}, 0)
	if err != nil {
		t.Fatal(err)
	}
	got, err := s.Get(o.ID)
	if err != nil || got.ID != o.ID || got.Status != StatusOpen {
		t.Fatalf("Get(%q) = %+v, %v", o.ID, got, err)
	}
}
