// tests/hier_adder.v flattened the way synthesis and place-and-route write it: the hierarchy is left only in
// escaped instance names, and some cells have tool names (_101_, place7) that say nothing about their module.
module hier_flat (clk, a, b, ci, q, qp);
  input clk;
  input [1:0] a, b;
  input ci;
  output [2:0] q;
  output qp;
  wire [1:0] s;
  wire co, p, pn, c0, t, t_b;
  wire [2:0] qn;
  wire f0s1, f0c1, f0c2, f1s1, f1c1, f1c2;
  XOR2xp5_ASAP7_75t_R \u_add/fa0/h0/x  (.A(a[0]), .B(b[0]), .Y(f0s1));
  AND2x2_ASAP7_75t_R \u_add/fa0/h0/n  (.A(a[0]), .B(b[0]), .Y(f0c1));
  XOR2xp5_ASAP7_75t_R \u_add/fa0/h1/x  (.A(f0s1), .B(ci), .Y(s[0]));
  AND2x2_ASAP7_75t_R \u_add/fa0/h1/n  (.A(f0s1), .B(ci), .Y(f0c2));
  OR2x2_ASAP7_75t_R _101_ (.A(f0c1), .B(f0c2), .Y(c0));
  XOR2xp5_ASAP7_75t_R \u_add/fa1/h0/x  (.A(a[1]), .B(b[1]), .Y(f1s1));
  AND2x2_ASAP7_75t_R \u_add/fa1/h0/n  (.A(a[1]), .B(b[1]), .Y(f1c1));
  XOR2xp5_ASAP7_75t_R \u_add/fa1/h1/x  (.A(f1s1), .B(c0), .Y(s[1]));
  AND2x2_ASAP7_75t_R \u_add/fa1/h1/n  (.A(f1s1), .B(c0), .Y(f1c2));
  OR2x2_ASAP7_75t_R _102_ (.A(f1c1), .B(f1c2), .Y(co));
  XOR2xp5_ASAP7_75t_R \u_par/x0  (.A(s[0]), .B(s[1]), .Y(t));
  BUFx2_ASAP7_75t_R place7 (.A(t), .Y(t_b));
  XOR2xp5_ASAP7_75t_R \u_par/x1  (.A(t_b), .B(co), .Y(p));
  DFFHQNx1_ASAP7_75t_R \r0$_DFF_P_  (.CLK(clk), .D(s[0]), .QN(qn[0]));
  DFFHQNx1_ASAP7_75t_R \r1$_DFF_P_  (.CLK(clk), .D(s[1]), .QN(qn[1]));
  DFFHQNx1_ASAP7_75t_R \r2$_DFF_P_  (.CLK(clk), .D(co), .QN(qn[2]));
  DFFHQNx1_ASAP7_75t_R \rp$_DFF_P_  (.CLK(clk), .D(p), .QN(pn));
  INVx1_ASAP7_75t_R _103_ (.A(qn[0]), .Y(q[0]));
  INVx1_ASAP7_75t_R _104_ (.A(qn[1]), .Y(q[1]));
  INVx1_ASAP7_75t_R _105_ (.A(qn[2]), .Y(q[2]));
  INVx1_ASAP7_75t_R _106_ (.A(pn), .Y(qp));
endmodule
